//! Account lifecycle, drift and hardening (spec 0005 slice 6) end to end
//! against the stateful Jellyfin and Emby fakes: deletion when a deletion
//! order is due (D16), the rolling destructive cap (D15), deviations (D13),
//! admin notifications (D2, D10), retry safety of password writes (ADR
//! 0010). Every test asserts the exact requests other than `GET`.
#![allow(clippy::unwrap_used)]

use chrono::{TimeDelta, Utc};
use serde_json::json;
use starr_adapter_kit::contract::media_server::{FakeFlavor, Write};
use starr_core::jobs::JobKind;
use starr_core::provisioning::{Action, Gate, HoldReason, Plan, Reason};
use starr_db::notifications::Stream;

use crate::JobHandler;
use crate::account_writes_tests::{JF_MOVIES, Setup, ctx, setup};

impl Setup {
    async fn stored_plan(&self) -> Plan {
        let stored = self.db.account_plan(self.id).await.unwrap().unwrap();
        serde_json::from_str(&stored.plan).unwrap()
    }

    /// Schedule `member`'s deletion with the grace period ending
    /// `grace_minutes` from now (negative: over already).
    async fn delete_order(&self, member: i64, grace_minutes: i64) {
        let now = Utc::now();
        assert!(
            self.db
                .schedule_member_deletion(
                    member,
                    "root",
                    now + TimeDelta::minutes(grace_minutes),
                    now - TimeDelta::days(30),
                )
                .await
                .unwrap()
        );
    }

    /// Give every listed account exactly the Movies library in the fake
    /// and scan again, so linking writes nothing.
    async fn only_movies_everywhere(&self, movies: &str) {
        for user in self.fake.users() {
            let name = user["Name"].as_str().unwrap().to_owned();
            self.fake.edit_user(&name, |u| {
                u["Policy"]["EnableAllFolders"] = json!(false);
                u["Policy"]["EnabledFolders"] = json!([movies]);
            });
        }
        self.scan().await;
    }

    /// Treat the member's linked account as one Starr created (origin
    /// `provisioned`, Starr password written), as after a create.
    async fn as_created(&self, member: i64) {
        sqlx::query(
            "UPDATE service_account SET origin = 'provisioned', decision = 'create',
                 password = 'starr', password_generation_written = 0
             WHERE member_id = ? AND service_instance_id = ?",
        )
        .bind(member)
        .bind(self.id)
        .execute(self.db.pool())
        .await
        .unwrap();
    }

    async fn scan(&self) {
        self.jobs
            .run(JobKind::AccountsScan, ctx(&self.db))
            .await
            .unwrap();
    }
}

/// D16 on both services (lesson 4): the member whose deletion order is due
/// loses the account Starr created (exactly one `DELETE`); an adopted
/// account of a due member is only disabled and then released, never
/// deleted (maintainer 2026-11-03); a member still in the grace period is
/// only disabled. Starr forgets the deleted account and audits it.
#[tokio::test]
async fn a_due_deletion_order_deletes_exactly_the_members_accounts() {
    for flavor in [FakeFlavor::Jellyfin, FakeFlavor::Emby] {
        let s = setup(flavor, 2).await;
        let movies = match flavor {
            FakeFlavor::Jellyfin => JF_MOVIES,
            FakeFlavor::Emby => crate::account_writes_tests::EMBY_MOVIES,
        };
        s.only_movies_everywhere(movies).await;
        let friend = s.linked("friend", "friend").await;
        s.as_created(friend).await;
        let alex = s.linked("alex", "friend-1").await;
        let sam = s.linked("sam", "friend-2").await;
        s.confirm().await;
        s.reconcile().await;
        assert_eq!(s.fake.writes().await, vec![], "{flavor:?}: in sync");
        let friend_id = s.remote_id("friend");
        let alex_id = s.remote_id("friend-1");
        let sam_id = s.remote_id("friend-2");
        s.delete_order(friend, -1).await;
        s.delete_order(alex, 60 * 24 * 30).await;
        s.delete_order(sam, -1).await;
        let report = s.reconcile().await;
        assert_eq!(report.counter("delete"), Some(1), "{report:?}");
        let writes = s.fake.writes().await;
        assert_eq!(writes.len(), 3, "{flavor:?}: {writes:?}");
        assert_eq!(writes.iter().filter(|w| w.method == "DELETE").count(), 1);
        assert!(writes.contains(&Write {
            method: "DELETE".into(),
            target: format!("/Users/{friend_id}"),
            body: serde_json::Value::Null,
        }));
        for id in [&alex_id, &sam_id] {
            let disable = writes
                .iter()
                .find(|w| w.target == format!("/Users/{id}/Policy"))
                .unwrap();
            assert_eq!(disable.body["IsDisabled"], json!(true));
        }
        assert!(s.fake.user("friend").is_none(), "{flavor:?}: deleted");
        assert!(s.fake.user("friend-1").is_some());
        assert!(s.fake.user("friend-2").is_some(), "adopted: never deleted");
        // Starr forgot it; the audit names the outcome.
        assert!(!s.db.member_owns_accounts(friend).await.unwrap());
        assert!(s.db.member_owns_accounts(alex).await.unwrap());
        let audit = s.audit_actions().await;
        let (action, details) = audit.iter().find(|(a, _)| a == "account.delete").unwrap();
        assert_eq!(action, "account.delete");
        assert_eq!(details["result"], json!("ok"));
        assert_eq!(details["remoteId"], json!(friend_id));
        assert_eq!(details["reason"], json!("deletionDue"));
        // Disabled, the adopted account is released by the replan after
        // the run (still in the service, never deleted).
        assert!(!s.db.member_owns_accounts(sam).await.unwrap());
        let audit = s.audit_actions().await;
        let (_, released) = audit.iter().find(|(a, _)| a == "account.unlink").unwrap();
        assert_eq!(released["reason"], json!("adoptedAtDeletion"));
        assert_eq!(released["remoteId"], json!(sam_id));
        // Nothing more on the next runs, also after the tombstone.
        s.db.tombstone_due_members(Utc::now()).await.unwrap();
        s.scan().await;
        s.reconcile().await;
        assert_eq!(s.fake.writes().await.len(), 3, "{flavor:?}");
    }
}

/// A tombstone (housekeeping ran first) still loses its accounts; with
/// writes off, a due deletion is planned and nothing at all is sent.
#[tokio::test]
async fn tombstones_are_deleted_only_through_the_gate() {
    let s = setup(FakeFlavor::Jellyfin, 0).await;
    s.only_movies_everywhere(JF_MOVIES).await;
    let friend = s.linked("friend", "friend").await;
    s.as_created(friend).await;
    let id = s.remote_id("friend");
    s.delete_order(friend, -1).await;
    s.db.tombstone_due_members(Utc::now()).await.unwrap();
    s.reconcile().await;
    let plan = s.stored_plan().await;
    let entry = plan.entries.iter().find(|e| e.member_id == friend).unwrap();
    assert_eq!(entry.steps.len(), 1);
    assert_eq!(entry.steps[0].action, Action::Delete);
    assert_eq!(entry.steps[0].reason, Reason::DeletionDue);
    assert_eq!(s.fake.writes().await, vec![], "plan only: nothing sent");
    s.confirm().await;
    s.reconcile().await;
    assert_eq!(
        s.fake.writes().await,
        vec![Write {
            method: "DELETE".into(),
            target: format!("/Users/{id}"),
            body: serde_json::Value::Null,
        }]
    );
}

/// The rolling window (slice 6, deviation): six single disables in a row
/// do not pass the cap one by one; the sixth within the hour waits for an
/// admin's approval.
#[tokio::test]
async fn six_single_disables_in_a_row_halt_at_the_cap() {
    let s = setup(FakeFlavor::Jellyfin, 6).await;
    s.only_movies_everywhere(JF_MOVIES).await;
    let mut members = Vec::new();
    for i in 1..=6 {
        members.push(s.linked(&format!("m{i}"), &format!("friend-{i}")).await);
    }
    s.confirm().await;
    s.reconcile().await;
    assert_eq!(s.fake.writes().await, vec![]);
    for (i, member) in members.iter().enumerate() {
        s.db.suspend_member(*member).await.unwrap();
        s.reconcile().await;
        let written = s.fake.writes().await.len();
        if i < 5 {
            assert_eq!(written, i + 1, "disable {} runs", i + 1);
        } else {
            assert_eq!(written, 5, "the sixth waits");
        }
    }
    let stored = s.db.account_plan(s.id).await.unwrap().unwrap();
    let plan: Plan = serde_json::from_str(&stored.plan).unwrap();
    let p = crate::provisioning::Planned {
        service_id: s.id,
        service_name: "jellyfin".into(),
        counts: plan.counts(),
        plan,
        fingerprint: stored.fingerprint.clone(),
    };
    let gate = crate::provisioning::gate_of(&s.db, &p, Utc::now())
        .await
        .unwrap();
    assert_eq!(
        gate,
        Gate::Hold {
            reason: HoldReason::OverCap {
                destructive: 1,
                cap: 5,
                recent: 5,
            }
        }
    );
    // An admin approves exactly this plan: it runs.
    s.db.confirm_account_plan(s.id, &stored.fingerprint, "root", Utc::now())
        .await
        .unwrap();
    s.reconcile().await;
    assert_eq!(s.fake.writes().await.len(), 6);
}

/// D13: an account disabled by hand in the service while the member is
/// active is enabled again, and the step is marked as a deviation; one
/// never written by Starr is not a deviation.
#[tokio::test]
async fn a_change_made_in_the_service_is_undone_as_a_deviation() {
    let s = setup(FakeFlavor::Jellyfin, 1).await;
    s.only_movies_everywhere(JF_MOVIES).await;
    s.linked("friend", "friend").await;
    s.confirm().await;
    s.reconcile().await;
    assert_eq!(s.fake.writes().await, vec![]);
    // The live run found it in sync: that is the baseline.
    s.fake.edit_user("friend", |u| {
        u["Policy"]["IsDisabled"] = json!(true);
    });
    s.scan().await;
    let report = s.reconcile().await;
    assert_eq!(report.counter("drift"), Some(1), "{report:?}");
    let writes = s.fake.writes().await;
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].body["IsDisabled"], json!(false));
    // Linked now, disabled before Starr ever wrote: no deviation.
    s.fake.edit_user("friend-1", |u| {
        u["Policy"]["IsDisabled"] = json!(true);
    });
    s.scan().await;
    s.linked("alex", "friend-1").await;
    s.reconcile().await;
    let plan = s.stored_plan().await;
    assert!(
        plan.entries
            .iter()
            .flat_map(|e| &e.steps)
            .all(|step| !step.drift)
    );
}

/// D2/D10: admins hear once about a managed account that gained
/// administrator rights or was locked by the service.
#[tokio::test]
async fn admins_are_told_once_about_admin_rights_and_lockouts() {
    let s = setup(FakeFlavor::Jellyfin, 1).await;
    s.only_movies_everywhere(JF_MOVIES).await;
    s.linked("friend", "friend").await;
    s.linked("alex", "friend-1").await;
    s.fake.edit_user("friend", |u| {
        u["Policy"]["IsAdministrator"] = json!(true);
    });
    s.fake.edit_user("friend-1", |u| {
        u["Policy"]["IsDisabled"] = json!(true);
        u["Policy"]["LoginAttemptsBeforeLockout"] = json!(3);
        u["Policy"]["InvalidLoginAttemptCount"] = json!(4);
    });
    s.scan().await;
    for _ in 0..2 {
        s.reconcile().await;
    }
    let notes =
        s.db.list_notifications(Stream::Admin, None, 20)
            .await
            .unwrap();
    let mut kinds: Vec<&str> = notes.iter().map(|n| n.kind.as_str()).collect();
    kinds.sort_unstable();
    assert_eq!(kinds, vec!["account_locked", "account_privileged"]);
    let privileged = notes
        .iter()
        .find(|n| n.kind.as_str() == "account_privileged")
        .unwrap();
    assert_eq!(privileged.payload.member.as_deref(), Some("friend"));
    assert_eq!(privileged.payload.title, "jellyfin");
    assert_eq!(s.fake.writes().await, vec![], "plan only, nothing written");
}
/// A due deletion on a paused instance is planned and sends nothing; an
/// account that gained admin rights after the last scan is refused by the
/// adapter right before the delete (read first), so no `DELETE` goes out.
#[tokio::test]
async fn due_deletions_wait_while_paused_and_never_reach_an_administrator() {
    let s = setup(FakeFlavor::Jellyfin, 0).await;
    s.only_movies_everywhere(JF_MOVIES).await;
    let friend = s.linked("friend", "friend").await;
    s.as_created(friend).await;
    s.confirm().await;
    s.reconcile().await;
    s.db.pause_account_writes(s.id, "root", Utc::now())
        .await
        .unwrap();
    s.delete_order(friend, -1).await;
    s.reconcile().await;
    let plan = s.stored_plan().await;
    let entry = plan.entries.iter().find(|e| e.member_id == friend).unwrap();
    assert_eq!(entry.steps[0].action, Action::Delete);
    assert_eq!(s.fake.writes().await, vec![], "paused: nothing sent");
    // Admin rights gained in the service after the last scan.
    s.fake.edit_user("friend", |u| {
        u["Policy"]["IsAdministrator"] = json!(true);
    });
    let fingerprint = s.db.account_plan(s.id).await.unwrap().unwrap().fingerprint;
    s.db.confirm_account_plan(s.id, &fingerprint, "root", Utc::now())
        .await
        .unwrap();
    s.reconcile().await;
    assert_eq!(
        s.fake.writes().await,
        vec![],
        "never deletes an administrator"
    );
    assert!(s.fake.user("friend").is_some());
    let audit = s.audit_actions().await;
    let (_, details) = audit.iter().find(|(a, _)| a == "account.delete").unwrap();
    assert_eq!(details["result"], json!("failed"));
    assert_eq!(details["code"], json!("rejected"));
}
