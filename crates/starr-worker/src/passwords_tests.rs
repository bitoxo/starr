//! `accounts.password` (spec 0005 slice 5, ADR 0010) against the stateful
//! Jellyfin and Emby fakes seeded from the recorded fixtures: creates and
//! password writes from handoffs, only through the safe-mode gate, with the
//! exact requests asserted, and no password anywhere but in the request
//! that carries it.
#![allow(clippy::unwrap_used)]

use chrono::{TimeDelta, Utc};
use secrecy::SecretString;
use serde_json::json;
use starr_adapter_kit::contract::media_server::{FakeFlavor, Write};
use starr_core::jobs::{Expectation, JobKind, Report};
use starr_core::members::{Category, Entitlement, RequestMode};
use starr_core::provisioning::{Action, PasswordUse, Plan};
use starr_db::members::NewMember;

use crate::JobHandler;
use crate::account_writes_tests::{EMBY_MOVIES, JF_MOVIES, Setup, ctx, setup};
use crate::secrets::seal_password_handoff;

const PASSWORD: &str = "Kino abend 🎬 \"q\" \\ %20";

impl Setup {
    /// A new member entitled to movies, decided "create" on the instance
    /// (as for members created after the instance got accounts).
    async fn newcomer(&self, name: &str) -> i64 {
        let email = format!("{name}@example.org");
        let member = self
            .db
            .create_member(
                &NewMember {
                    username: name,
                    email: &email,
                    display_name: name,
                    password_hash: Some("$argon2id$dummy"),
                },
                &[Entitlement {
                    category: Category::Movies,
                    enabled: true,
                    request_mode: RequestMode::Approval,
                }],
                Utc::now(),
            )
            .await
            .unwrap();
        self.db
            .initial_service_accounts(member, &[self.id], &[], Utc::now())
            .await
            .unwrap();
        member
    }

    /// Seal `password` for `member` at the member's generation, as the web
    /// process does.
    async fn handoff(&self, member: i64, password: &str) {
        let generation = self
            .db
            .member_password_generation(member)
            .await
            .unwrap()
            .unwrap();
        let sealer =
            starr_secrets::CredentialSealer::parse(self.jobs.credentials.recipient()).unwrap();
        let (recipient, ciphertext) = seal_password_handoff(
            &sealer,
            member,
            generation,
            &SecretString::from(password.to_owned()),
        )
        .unwrap();
        assert!(
            self.db
                .store_password_handoff(member, generation, &recipient, &ciphertext, Utc::now())
                .await
                .unwrap()
        );
    }

    async fn passwords(&self) -> Report {
        self.jobs
            .run(JobKind::AccountsPassword, ctx(&self.db))
            .await
            .unwrap()
    }

    async fn plan(&self) -> Plan {
        let stored = self.db.account_plan(self.id).await.unwrap().unwrap();
        serde_json::from_str(&stored.plan).unwrap()
    }

    async fn row(&self, member: i64) -> starr_db::service_accounts::ServiceAccountRow {
        self.db
            .member_service_accounts(member)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.service_instance_id == self.id)
            .unwrap()
    }

    async fn notification_kinds(&self, member: i64) -> Vec<String> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT kind FROM notification WHERE member_id = ? ORDER BY id")
                .bind(member)
                .fetch_all(self.db.pool())
                .await
                .unwrap();
        rows.into_iter().map(|(k,)| k).collect()
    }

    /// Everything Starr stored that could hold a secret: audit, run
    /// reports, plans, errors.
    async fn stored_text(&self) -> String {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT coalesce(details, '') FROM audit_log
             UNION ALL SELECT plan FROM account_plan
             UNION ALL SELECT coalesce(last_error_detail, '') FROM service_account
             UNION ALL SELECT coalesce(payload, '') FROM notification",
        )
        .fetch_all(self.db.pool())
        .await
        .unwrap();
        rows.into_iter()
            .map(|(t,)| t)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn targets(writes: &[Write]) -> Vec<(String, String)> {
    writes
        .iter()
        .map(|w| (w.method.clone(), w.target.clone()))
        .collect()
}

#[tokio::test]
async fn with_writes_off_or_paused_a_handoff_writes_nothing_and_waits() {
    let s = setup(FakeFlavor::Jellyfin, 0).await;
    let mia = s.newcomer("mia").await;
    s.handoff(mia, PASSWORD).await;
    s.reconcile().await;
    let report = s.passwords().await;
    assert_eq!(report.counter("waiting"), Some(1), "{report:?}");
    assert_eq!(s.fake.writes().await, vec![], "plan only: nothing sent");
    assert!(s.db.has_password_handoff(mia, Utc::now()).await.unwrap());

    s.confirm().await;
    s.db.pause_account_writes(s.id, "root", Utc::now())
        .await
        .unwrap();
    s.passwords().await;
    assert_eq!(s.fake.writes().await, vec![], "paused: nothing sent");
    assert!(s.db.has_password_handoff(mia, Utc::now()).await.unwrap());
    // At the latest after an hour the handoff is gone.
    let later = Utc::now() + TimeDelta::hours(1);
    assert_eq!(
        s.db.delete_expired_password_handoffs(later).await.unwrap(),
        1
    );
}

#[tokio::test]
async fn a_new_jellyfin_member_is_created_with_the_password_and_the_handoff_deleted() {
    let s = setup(FakeFlavor::Jellyfin, 0).await;
    let mia = s.newcomer("mia").await;
    s.confirm().await;
    s.reconcile().await;
    // Activate: the account waits only for the password.
    let plan = s.plan().await;
    let entry = plan.entries.iter().find(|e| e.member_id == mia).unwrap();
    assert!(matches!(entry.steps[0].action, Action::Create { .. }));
    s.handoff(mia, PASSWORD).await;
    let report = s.passwords().await;
    assert!(
        matches!(report.expectation(), Expectation::Met(_)),
        "{report:?}"
    );
    assert_eq!(report.counter("created"), Some(1));
    let writes = s.fake.writes().await;
    let id = s.remote_id("mia");
    let policy = format!("/Users/{id}/Policy");
    assert_eq!(
        targets(&writes),
        vec![
            ("POST".into(), "/Users/New".into()),
            ("POST".into(), policy.clone()),
            ("POST".into(), policy),
        ]
    );
    assert_eq!(
        writes[0].body,
        json!({ "Name": "mia", "Password": PASSWORD })
    );
    assert_eq!(writes[1].body["EnabledFolders"], json!([JF_MOVIES]));
    assert_eq!(writes[2].body["IsDisabled"], json!(false));
    // Owned, with the password generation written; the handoff is gone.
    let row = s.row(mia).await;
    assert_eq!(row.remote_id.as_deref(), Some(id.as_str()));
    assert_eq!(row.password, Some(PasswordUse::Starr));
    assert_eq!(row.password_written, Some(0));
    assert!(!s.db.has_password_handoff(mia, Utc::now()).await.unwrap());
    let audit = s.audit_actions().await;
    let (action, details) = audit.last().unwrap();
    assert_eq!(action, "account.create");
    assert_eq!(details["result"], json!("ok"));
    assert!(!s.stored_text().await.contains("Kino abend"));

    // Nothing is left: the next plan and password runs write nothing.
    s.reconcile().await;
    s.passwords().await;
    assert_eq!(s.fake.writes().await.len(), 3);
}

#[tokio::test]
async fn emby_creates_in_the_safe_order_and_only_with_a_password() {
    let s = setup(FakeFlavor::Emby, 0).await;
    let mia = s.newcomer("mia").await;
    s.confirm().await;
    s.reconcile().await;
    s.handoff(mia, PASSWORD).await;
    s.passwords().await;
    let writes = s.fake.writes().await;
    let id = s.remote_id("mia");
    let policy = format!("/Users/{id}/Policy");
    assert_eq!(
        targets(&writes),
        vec![
            ("POST".into(), "/Users/New".into()),
            ("POST".into(), policy.clone()),
            ("POST".into(), format!("/Users/{id}/Password")),
            ("POST".into(), policy),
        ]
    );
    assert_eq!(writes[0].body, json!({ "Name": "mia" }));
    assert_eq!(writes[1].body["IsDisabled"], json!(true));
    assert_eq!(writes[1].body["EnabledFolders"], json!([EMBY_MOVIES]));
    assert_eq!(writes[2].body, json!({ "NewPw": PASSWORD }));
    assert_eq!(writes[3].body["IsDisabled"], json!(false));
    assert_eq!(s.row(mia).await.password_written, Some(0));
}

#[tokio::test]
async fn an_adopted_account_switches_only_when_the_member_asks() {
    let s = setup(FakeFlavor::Emby, 0).await;
    let friend = s.linked("friend", "friend").await;
    s.confirm().await;
    s.reconcile().await;
    let before = s.fake.writes().await.len();
    // A handoff alone (e.g. a password change) leaves the old password.
    s.db.change_member_password(friend, "h", &[0; 32])
        .await
        .unwrap();
    s.handoff(friend, PASSWORD).await;
    s.passwords().await;
    assert_eq!(s.fake.writes().await.len(), before, "deferred stays");
    assert!(!s.db.has_password_handoff(friend, Utc::now()).await.unwrap());

    // "Use my Starr password here" (M2).
    s.db.request_password_switch(friend, s.id).await.unwrap();
    s.handoff(friend, PASSWORD).await;
    s.reconcile().await;
    let report = s.passwords().await;
    assert_eq!(report.counter("passwords"), Some(1), "{report:?}");
    let writes = s.fake.writes().await;
    let id = s.remote_id("friend");
    assert_eq!(
        writes[before..].to_vec(),
        vec![Write {
            method: "POST".into(),
            target: format!("/Users/{id}/Password"),
            body: json!({ "NewPw": PASSWORD }),
        }]
    );
    let row = s.row(friend).await;
    assert_eq!(
        (row.password, row.password_written),
        (Some(PasswordUse::Starr), Some(1))
    );
    let audit = s.audit_actions().await;
    let (action, details) = audit.last().unwrap();
    assert_eq!(action, "account.password");
    assert_eq!(details["result"], json!("ok"));
    assert!(!s.stored_text().await.contains("Kino abend"));
}

#[tokio::test]
async fn a_password_change_is_written_where_the_starr_password_is_used() {
    let s = setup(FakeFlavor::Jellyfin, 0).await;
    let mia = s.newcomer("mia").await;
    let sam = s.newcomer("sam").await;
    s.confirm().await;
    s.reconcile().await;
    s.handoff(mia, PASSWORD).await;
    s.handoff(sam, "sam's first password").await;
    s.passwords().await;
    let before = s.fake.writes().await.len();
    // Mia changes her password: only her account is due (lesson 4).
    let generation =
        s.db.change_member_password(mia, "h2", &[0; 32])
            .await
            .unwrap();
    s.reconcile().await;
    s.handoff(mia, "a brand new password").await;
    s.passwords().await;
    let writes = s.fake.writes().await;
    let id = s.remote_id("mia");
    assert_eq!(
        writes[before..].to_vec(),
        vec![Write {
            method: "POST".into(),
            target: format!("/Users/Password?userId={id}"),
            body: json!({ "NewPw": "a brand new password" }),
        }]
    );
    assert_eq!(s.row(mia).await.password_written, Some(generation));
    assert_eq!(s.row(sam).await.password_written, Some(0));
}

#[tokio::test]
async fn a_held_plan_holds_the_handoff_too() {
    // Six members to disable: over the cap, the live plan waits for approval.
    let s = setup(FakeFlavor::Jellyfin, 6).await;
    let mut others = Vec::new();
    for i in 1..=6 {
        others.push(s.linked(&format!("f{i}"), &format!("friend-{i}")).await);
    }
    let mia = s.newcomer("mia").await;
    s.confirm().await;
    s.reconcile().await;
    let before = s.fake.writes().await.len();
    for member in others {
        s.db.suspend_member(member).await.unwrap();
    }
    s.reconcile().await;
    s.handoff(mia, PASSWORD).await;
    let report = s.passwords().await;
    assert_eq!(report.counter("waiting"), Some(1), "{report:?}");
    assert_eq!(s.fake.writes().await.len(), before, "held: nothing sent");
    assert!(s.db.has_password_handoff(mia, Utc::now()).await.unwrap());
}

#[tokio::test]
async fn no_password_is_written_for_a_suspended_member() {
    let s = setup(FakeFlavor::Jellyfin, 0).await;
    let mia = s.newcomer("mia").await;
    s.confirm().await;
    s.reconcile().await;
    s.handoff(mia, PASSWORD).await;
    // Suspending drops the handoff (D9).
    s.db.suspend_member(mia).await.unwrap();
    assert!(!s.db.has_password_handoff(mia, Utc::now()).await.unwrap());
    s.passwords().await;
    assert_eq!(s.fake.writes().await, vec![]);
}

#[tokio::test]
async fn an_unfinished_emby_create_stays_disabled_until_the_password_arrives() {
    let s = setup(FakeFlavor::Emby, 0).await;
    let mia = s.newcomer("mia").await;
    s.confirm().await;
    s.reconcile().await;
    s.fake.fail_policy_writes();
    s.handoff(mia, PASSWORD).await;
    let report = s.passwords().await;
    assert!(
        matches!(report.expectation(), Expectation::Unmet(_)),
        "{report:?}"
    );
    // Owned, never written: "needs you", and the handoff stays for a retry
    // (the service was in trouble).
    let id = s.remote_id("mia");
    let row = s.row(mia).await;
    assert_eq!(row.remote_id.as_deref(), Some(id.as_str()));
    assert_eq!(row.password_written, None);
    let missing = s.db.accounts_without_password().await.unwrap();
    assert_eq!(missing.len(), 1);
    assert!(s.db.has_password_handoff(mia, Utc::now()).await.unwrap());
    // The account got a random password, never the member's or an empty one.
    let user = s.fake.user("mia").unwrap();
    assert_eq!(user["HasPassword"], json!(true));
    assert_eq!(user["Policy"]["IsDisabled"], json!(false), "policy refused");

    // The service recovers: the plan disables first, writes the password,
    // and only then enables (never enabled without one).
    s.fake.allow_policy_writes();
    s.jobs.run(JobKind::AccountsScan, ctx(&s.db)).await.unwrap();
    let before = s.fake.writes().await.len();
    s.reconcile().await;
    s.passwords().await;
    let writes = s.fake.writes().await[before..].to_vec();
    let policy = format!("/Users/{id}/Policy");
    let password = format!("/Users/{id}/Password");
    let got = targets(&writes);
    let disable = got.iter().position(|t| t.1 == policy).unwrap();
    let pw = got.iter().position(|t| t.1 == password).unwrap();
    assert!(disable < pw, "disabled before the password: {got:?}");
    assert_eq!(writes[disable].body["IsDisabled"], json!(true));
    assert_eq!(writes[pw].body, json!({ "NewPw": PASSWORD }));
    assert_eq!(s.db.accounts_without_password().await.unwrap(), vec![]);
    s.reconcile().await;
    let user = s.fake.user("mia").unwrap();
    assert_eq!(user["Policy"]["IsDisabled"], json!(false), "enabled last");
    assert!(!s.stored_text().await.contains("Kino abend"));
}

#[tokio::test]
async fn members_waiting_for_their_password_are_told_once() {
    let s = setup(FakeFlavor::Jellyfin, 0).await;
    let mia = s.newcomer("mia").await;
    s.reconcile().await;
    // Plan only: nobody is asked for a password that could not be used.
    assert_eq!(s.notification_kinds(mia).await, Vec::<String>::new());
    s.confirm().await;
    s.reconcile().await;
    s.reconcile().await;
    let kinds = s.notification_kinds(mia).await;
    assert_eq!(kinds, vec!["account_activate".to_owned()]);
}
/// Retry safety (slice 6): a job retried after the password was written
/// but before its handoff was deleted writes nothing again, and a write
/// interrupted before its result was recorded is never repeated, so the
/// member's devices are not signed out twice.
#[tokio::test]
async fn a_retried_password_job_never_writes_the_same_password_twice() {
    let s = setup(FakeFlavor::Emby, 0).await;
    let friend = s.linked("friend", "friend").await;
    s.confirm().await;
    s.reconcile().await;
    s.db.request_password_switch(friend, s.id).await.unwrap();
    s.handoff(friend, PASSWORD).await;
    s.reconcile().await;
    s.passwords().await;
    let after_first = s.fake.writes().await.len();
    // The same handoff again, as if the job died before deleting it.
    s.handoff(friend, PASSWORD).await;
    s.passwords().await;
    assert_eq!(s.fake.writes().await.len(), after_first, "no second write");
    assert!(!s.db.has_password_handoff(friend, Utc::now()).await.unwrap());

    // A new password whose write was started but never recorded (the
    // worker crashed in between): not written again, the member is asked
    // to enter it once more.
    let generation =
        s.db.change_member_password(friend, "h2", &[0; 32])
            .await
            .unwrap();
    s.reconcile().await;
    let remote = s.remote_id("friend");
    assert!(
        s.db.begin_password_attempt(s.id, friend, &remote, generation)
            .await
            .unwrap()
    );
    s.handoff(friend, "another password").await;
    let report = s.passwords().await;
    assert_eq!(report.counter("passwords"), Some(0), "{report:?}");
    assert_eq!(s.fake.writes().await.len(), after_first, "never repeated");
    assert!(!s.db.has_password_handoff(friend, Utc::now()).await.unwrap());
    let audit = s.audit_actions().await;
    let (action, details) = audit.last().unwrap();
    assert_eq!(action, "account.password");
    assert_eq!(details["result"], json!("interrupted"));
    // The member's next entry (same password, same generation) writes it:
    // only the automatic repeat is refused.
    s.reconcile().await;
    s.handoff(friend, "the third password").await;
    s.passwords().await;
    assert_eq!(s.fake.writes().await.len(), after_first + 1);
    let row = s.row(friend).await;
    assert_eq!(
        (row.password_written, row.password_attempt),
        (Some(generation), None)
    );
    assert!(!s.stored_text().await.contains("another password"));
}

/// An account created while an admin changed the member's decision (it
/// is no longer the member's when the create returns) is disabled at once
/// and left unowned: never usable with the member's password (slice 5
/// open point).
#[tokio::test]
async fn an_account_no_longer_owned_when_its_create_returns_is_disabled() {
    let s = setup(FakeFlavor::Jellyfin, 0).await;
    let mia = s.newcomer("mia").await;
    let sam = s.newcomer("sam").await;
    s.confirm().await;
    s.reconcile().await;
    // The fake hands out IDs in order: the next account is this one. Sam
    // owns it in Starr's records by the time the create returns.
    let next = format!("{:032x}", 0xfa4e_0000_u64 + 1);
    sqlx::query(
        "UPDATE service_account SET decision = 'linked', origin = 'adopted', remote_id = ?,
             password = 'deferred'
         WHERE member_id = ? AND service_instance_id = ?",
    )
    .bind(&next)
    .bind(sam)
    .bind(s.id)
    .execute(s.db.pool())
    .await
    .unwrap();
    s.handoff(mia, PASSWORD).await;
    s.passwords().await;
    let user = s.fake.user("mia").unwrap();
    assert_eq!(user["Id"], json!(next));
    assert_eq!(
        user["Policy"]["IsDisabled"],
        json!(true),
        "disabled at once"
    );
    let writes = s.fake.writes().await;
    let last = writes.last().unwrap();
    assert_eq!(last.target, format!("/Users/{next}/Policy"));
    assert_eq!(last.body["IsDisabled"], json!(true));
    assert_eq!(s.row(mia).await.remote_id, None, "not mia's");
    let audit = s.audit_actions().await;
    let (_, details) = audit
        .iter()
        .rev()
        .find(|(a, _)| a == "account.create")
        .unwrap();
    assert_eq!(details["result"], json!("unowned"));
}
