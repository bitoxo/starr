//! An older binary on a newer schema (ADR 0008 rollback, ADR 0012): values
//! a newer release wrote that this binary does not know leave their row
//! out of lists instead of failing them. Request lists are covered in
//! `requests_tests.rs`.
#![allow(clippy::unwrap_used)]

use starr_core::adapters::SecretClass;
use starr_core::members::{Category, Entitlement};

use crate::members::{MemberFilter, NewMember};
use crate::notifications::{ChannelUpdate, Stream};
use crate::runs::RunFilter;
use crate::services::{NewServiceInstance, StoredCredential};
use crate::test_support::{t0, write_as_newer_release};
use crate::{Database, ts};
use starr_core::notifications::NotificationKind;

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

async fn service(db: &Database, name: &str) -> i64 {
    db.create_service(
        &NewServiceInstance {
            adapter_type: "jellyfin",
            name,
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

#[tokio::test]
async fn notifications_with_unknown_kinds_are_left_out() {
    let db = Database::open_in_memory().await.unwrap();
    let mia = member(&db, "mia").await;
    let at = ts(t0());
    write_as_newer_release(
        &db,
        &format!(
            "INSERT INTO notification (member_id, kind, payload, dedup_key, created_at)
             VALUES ({mia}, 'request_available', '{{\"title\":\"A\",\"category\":\"movies\"}}', 'a', '{at}'),
                    ({mia}, 'request_teleported', '{{\"title\":\"B\",\"category\":\"movies\",\"later\":1}}', 'b', '{at}');
             INSERT INTO member_notification_pref (member_id, kind, email)
             VALUES ({mia}, 'request_available', 1), ({mia}, 'request_teleported', 1);"
        ),
    )
    .await;
    let listed = db
        .list_notifications(Stream::Member(mia), None, 50)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].payload.title, "A");
    assert_eq!(db.email_preferences(mia).await.unwrap().len(), 1);
}

#[tokio::test]
async fn runs_with_unknown_states_are_left_out() {
    let db = Database::open_in_memory().await.unwrap();
    let at = ts(t0());
    write_as_newer_release(
        &db,
        &format!(
            "INSERT INTO run (job_id, kind, trigger, attempt, state, started_at, finished_at)
             VALUES (1, 'inventory.sync', 'schedule', 1, 'succeeded', '{at}', '{at}'),
                    (2, 'accounts.reconcile', 'schedule', 1, 'paused', '{at}', '{at}'),
                    (3, 'accounts.scan', 'webhook', 1, 'running', '{at}', NULL);"
        ),
    )
    .await;
    let runs = db
        .list_runs(RunFilter {
            kind: None,
            state: None,
            job_id: None,
            outcome: None,
            before: None,
            limit: 50,
        })
        .await
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].kind, "inventory.sync");
}

#[tokio::test]
async fn categories_roles_entitlements_and_members_skip_unknown_values() {
    let db = Database::open_in_memory().await.unwrap();
    let mia = member(&db, "mia").await;
    let noah = member(&db, "noah").await;
    let jellyfin = service(&db, "Jellyfin").await;
    let at = ts(t0());
    write_as_newer_release(
        &db,
        &format!(
            "INSERT INTO category (name, enabled, updated_at) VALUES ('podcasts', 1, '{at}');
             INSERT INTO service_role (category, kind, service_instance_id, created_at, updated_at)
             VALUES ('movies', 'consume', {jellyfin}, '{at}', '{at}'),
                    ('podcasts', 'consume', {jellyfin}, '{at}', '{at}'),
                    ('movies', 'mirror', {jellyfin}, '{at}', '{at}');
             INSERT INTO entitlement (member_id, category, enabled, request_mode, updated_at)
             VALUES ({mia}, 'podcasts', 1, 'auto', '{at}');
             UPDATE entitlement SET enabled = 1, request_mode = 'instant'
                 WHERE member_id = {mia} AND category = 'movies';
             UPDATE member SET status = 'hibernating' WHERE id = {noah};"
        ),
    )
    .await;
    let categories = db.list_categories().await.unwrap();
    assert_eq!(categories.len(), Category::ALL.len());
    let roles = db.list_roles(None).await.unwrap();
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0].category, Category::Movies);
    // Unknown entries fall back to the defaults (off, approval).
    let entitlements = db.entitlements(mia).await.unwrap();
    assert_eq!(entitlements.len(), Category::ALL.len());
    assert!(entitlements.iter().all(|e| !e.enabled));
    let members = db
        .list_members(MemberFilter {
            status: None,
            query: None,
        })
        .await
        .unwrap();
    assert_eq!(members.iter().map(|m| m.id).collect::<Vec<_>>(), vec![mia]);
}

/// Writes keep what they cannot read: a channel update keeps admin events
/// of kinds a newer release added, saving email preferences keeps theirs.
#[tokio::test]
async fn updates_keep_values_of_a_newer_release() {
    let db = Database::open_in_memory().await.unwrap();
    let mia = member(&db, "mia").await;
    let at = ts(t0());
    write_as_newer_release(
        &db,
        &format!(
            "INSERT INTO notification_channel (id, kind, name, secret_ciphertext,
                 secret_recipient, admin_events, created_at, updated_at)
             VALUES (1, 'webhook', 'hook', x'00', 'age1x',
                 '[\"acquisition_failed\", \"account_conflict\"]', '{at}', '{at}');
             INSERT INTO member_notification_pref (member_id, kind, email)
             VALUES ({mia}, 'request_available', 1), ({mia}, 'request_teleported', 1);"
        ),
    )
    .await;
    let channel = db
        .update_channel(
            1,
            &ChannelUpdate {
                admin_events: Some(&[NotificationKind::ApprovalPending]),
                ..ChannelUpdate::default()
            },
            t0(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        channel.admin_events,
        vec![NotificationKind::ApprovalPending]
    );
    let (stored,): (String,) =
        sqlx::query_as("SELECT admin_events FROM notification_channel WHERE id = 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(stored, r#"["account_conflict","approval_pending"]"#);

    db.set_email_preferences(mia, &[NotificationKind::RequestApproved])
        .await
        .unwrap();
    let kinds: Vec<(String,)> = sqlx::query_as(
        "SELECT kind FROM member_notification_pref WHERE member_id = ? ORDER BY kind",
    )
    .bind(mia)
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        kinds,
        vec![("request_approved".into(),), ("request_teleported".into(),)]
    );
}

/// Deliveries of kinds a newer release added never fill the page of due
/// deliveries, so the known ones behind them still go out.
#[tokio::test]
async fn unknown_deliveries_do_not_block_the_queue() {
    let db = Database::open_in_memory().await.unwrap();
    let at = ts(t0());
    write_as_newer_release(
        &db,
        &format!(
            "INSERT INTO notification_channel (id, kind, name, secret_ciphertext,
                 secret_recipient, admin_events, created_at, updated_at)
             VALUES (1, 'webhook', 'hook', x'00', 'age1x', '[]', '{at}', '{at}'),
                    (2, 'matrix', 'room', x'00', 'age1x', '[]', '{at}', '{at}');
             INSERT INTO notification (id, member_id, kind, payload, dedup_key, created_at)
             VALUES (1, NULL, 'acquisition_failed', '{{\"title\":\"A\",\"category\":\"movies\"}}', 'a', '{at}'),
                    (2, NULL, 'account_conflict', '{{\"title\":\"B\",\"category\":\"movies\"}}', 'b', '{at}');
             INSERT INTO notification_delivery (id, notification_id, channel_id, state,
                 next_attempt_at, created_at, updated_at)
             VALUES (1, 2, 1, 'pending', '2026-01-01T00:00:00.000Z', '{at}', '{at}'),
                    (2, 1, 2, 'pending', '2026-01-01T00:00:00.000Z', '{at}', '{at}'),
                    (3, 1, 1, 'pending', NULL, '{at}', '{at}');"
        ),
    )
    .await;
    let due = db.due_deliveries(t0(), 1).await.unwrap();
    assert_eq!(due.iter().map(|d| d.id).collect::<Vec<_>>(), vec![3]);
    sqlx::query("UPDATE notification_delivery SET state = 'delivered' WHERE id = 3")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(!db.has_due_deliveries(t0()).await.unwrap());
}

/// The rebuilt inventory with album rows (spec 0006 slice 5) under 0.3.0's
/// own statements (copied from `v0.3.0:crates/starr-db/src/inventory.rs`):
/// its read parses every row, album rows included (`seasons` stays `{}`;
/// the track count is a column it never names), and its upsert can mark an
/// album gone. This release then reads the row without a stale count.
/// An inventory row as 0.3.0 selects it.
type OldRow = (
    String,
    String,
    Option<i64>,
    Option<i64>,
    Option<String>,
    String,
    Option<String>,
    String,
    Option<i64>,
    String,
    Option<String>,
    String,
    i64,
    Option<String>,
    Option<String>,
);

#[tokio::test]
async fn an_older_binary_reads_and_marks_album_rows_gone() {
    use std::collections::BTreeMap;

    use crate::inventory::{InventoryRow, album_key};

    let db = Database::open_in_memory().await.unwrap();
    let jellyfin = service(&db, "Jellyfin").await;
    let rg = uuid::Uuid::from_u128(0xb139_2450_e666_3926_a536_22c6_5f83_4433);
    let album = InventoryRow {
        natural_key: album_key(rg),
        kind: "album".to_owned(),
        tmdb_id: None,
        tvdb_id: None,
        imdb_id: None,
        remote_id: "a1".to_owned(),
        server_id: Some("srv".to_owned()),
        title: "OK Computer".to_owned(),
        year: Some(1997),
        seasons: BTreeMap::new(),
        present_since: Some(t0()),
        last_seen: t0(),
        missing_runs: 0,
        gone_since: None,
        library_ids: Some(vec!["music".to_owned()]),
        tracks: Some(12),
    };
    assert!(db.store_inventory(jellyfin, &[album]).await.unwrap());

    // 0.3.0's read.
    let rows: Vec<OldRow> = sqlx::query_as(
        "SELECT natural_key, kind, tmdb_id, tvdb_id, imdb_id, remote_id, server_id, title,
                year, seasons, present_since, last_seen, missing_runs, gone_since, library_ids
         FROM inventory WHERE service_instance_id = ? ORDER BY natural_key",
    )
    .bind(jellyfin)
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    let seasons: BTreeMap<u32, u32> = serde_json::from_str(&rows[0].9).unwrap();
    assert!(seasons.is_empty());

    // 0.3.0's upsert, marking the album gone (it lists no music library).
    let at = ts(t0());
    sqlx::query(
        "INSERT INTO inventory (service_instance_id, natural_key, kind, tmdb_id, tvdb_id,
             imdb_id, remote_id, server_id, title, year, seasons, present_since, last_seen,
             missing_runs, gone_since, library_ids)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (service_instance_id, natural_key) DO UPDATE SET
             kind = excluded.kind,
             tmdb_id = excluded.tmdb_id,
             tvdb_id = excluded.tvdb_id,
             imdb_id = excluded.imdb_id,
             remote_id = excluded.remote_id,
             server_id = excluded.server_id,
             title = excluded.title,
             year = excluded.year,
             seasons = excluded.seasons,
             present_since = excluded.present_since,
             last_seen = excluded.last_seen,
             missing_runs = excluded.missing_runs,
             gone_since = excluded.gone_since,
             library_ids = excluded.library_ids",
    )
    .bind(jellyfin)
    .bind(&rows[0].0)
    .bind(&rows[0].1)
    .bind(None::<i64>)
    .bind(None::<i64>)
    .bind(None::<String>)
    .bind("a1")
    .bind("srv")
    .bind("OK Computer")
    .bind(1997)
    .bind("{}")
    .bind(None::<String>)
    .bind(&at)
    .bind(2)
    .bind(&at)
    .bind(r#"["music"]"#)
    .execute(db.pool())
    .await
    .unwrap();

    let row = &db.inventory(jellyfin).await.unwrap()[0];
    assert!(row.gone_since.is_some());
    assert_eq!(row.tracks, None, "the count of a gone row is not read");
}
