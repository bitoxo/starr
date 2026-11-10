//! `accounts.reconcile` with writes (spec 0005 slice 4): safe mode end to
//! end against stateful Jellyfin and Emby fakes seeded from the recorded
//! fixtures. Every test asserts the exact requests other than `GET` the
//! worker sent, and that nothing at all is written while writes are off,
//! paused, held or the plan is incomplete.
#![allow(clippy::unwrap_used)]

use chrono::{TimeDelta, Utc};
use secrecy::SecretString;
use serde_json::{Value, json};
use starr_adapter_kit::AuthScheme;
use starr_adapter_kit::contract::media_server::{FakeFlavor, MediaServerFake, Recorded, Write};
use starr_core::jobs::{Expectation, JobKind};
use starr_core::members::{Category, Entitlement, RequestMode};
use starr_db::Database;
use starr_db::account_writes::ConfirmOutcome;
use starr_db::members::NewMember;
use starr_db::service_accounts::DecisionChange;
use starr_db::services::{NewServiceInstance, StoredCredential};
use tokio_util::sync::CancellationToken;

use crate::secrets::CredentialKey;
use crate::{JobContext, JobHandler, SystemJobs};

pub(crate) const KEY: &str = "writes-key-0123456789abcdef";
pub(crate) const JF_MOVIES: &str = "f137a2dd21bbc1b99aa5c0f6bf02a805";
pub(crate) const EMBY_MOVIES: &str = "c26fd8b5c1644eaaac60078a62ba4d00";

pub(crate) struct Setup {
    pub(crate) db: Database,
    pub(crate) jobs: SystemJobs,
    pub(crate) fake: MediaServerFake,
    pub(crate) id: i64,
}

pub(crate) fn ctx(db: &Database) -> JobContext {
    JobContext {
        db: db.clone(),
        payload: json!({}),
        cancel: CancellationToken::new(),
    }
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

/// A Jellyfin or Emby fake with `extra` copies of the recorded "friend"
/// (named `friend-1`…), an instance for it with a movies consume role
/// mapped to the fixture's Movies library, and one scan.
pub(crate) async fn setup(flavor: FakeFlavor, extra: usize) -> Setup {
    let (adapter, version, auth, movies) = match flavor {
        FakeFlavor::Jellyfin => (
            "jellyfin",
            "12.3.0",
            AuthScheme::MediaBrowserToken,
            JF_MOVIES,
        ),
        FakeFlavor::Emby => (
            "emby",
            "4.10.1.0",
            AuthScheme::ApiKeyHeader("X-Emby-Token"),
            EMBY_MOVIES,
        ),
    };
    let dir = format!(
        "{}/../adapters/{adapter}/fixtures/{version}/accounts",
        env!("CARGO_MANIFEST_DIR")
    );
    let fake = MediaServerFake::start(flavor, auth, KEY, Recorded::from_dir(&dir), "friend").await;
    let template = fake.user("friend").unwrap();
    for i in 1..=extra {
        let mut user = template.clone();
        user["Name"] = json!(format!("friend-{i}"));
        user["Id"] = json!(format!("{:032x}", 0xf00d_0000_u64 + i as u64));
        fake.add_user(user);
    }
    let db = Database::open_in_memory().await.unwrap();
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = db
        .create_service(
            &NewServiceInstance {
                adapter_type: adapter,
                name: adapter,
                base_url: &fake.server.uri(),
                public_url: None,
                credential: &seal(&jobs.credentials, KEY),
            },
            Utc::now(),
        )
        .await
        .unwrap()
        .id;
    db.add_role(
        &starr_db::categories::NewRole {
            category: Category::Movies,
            kind: starr_core::categories::RoleKind::Consume,
            service_instance_id: id,
            offered: &[starr_core::adapters::Capability::Libraries],
        },
        Utc::now(),
    )
    .await
    .unwrap();
    let role = db.list_roles(None).await.unwrap().remove(0);
    let grant = movies.to_owned();
    let item_id = match flavor {
        FakeFlavor::Jellyfin => movies.to_owned(),
        FakeFlavor::Emby => "3".to_owned(),
    };
    db.set_consume_libraries(
        role.id,
        &starr_core::libraries::ConsumeSettings {
            libraries: vec![starr_core::libraries::MappedLibrary {
                id: item_id,
                grant_id: grant,
            }],
        },
        Utc::now(),
    )
    .await
    .unwrap();
    db.set_category_enabled(Category::Movies, true, Utc::now())
        .await
        .unwrap();
    let s = Setup { db, jobs, fake, id };
    s.jobs.run(JobKind::AccountsScan, ctx(&s.db)).await.unwrap();
    s
}

impl Setup {
    /// A member entitled to movies, linked to the account named `account`.
    pub(crate) async fn linked(&self, name: &str, account: &str) -> i64 {
        let email = format!("{name}@example.org");
        let member = self
            .db
            .create_member(
                &NewMember {
                    username: name,
                    email: &email,
                    display_name: name,
                    password_hash: None,
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
        let remote_id = self.remote_id(account);
        self.db
            .decide_service_account(
                member,
                self.id,
                DecisionChange::Link {
                    remote_id: &remote_id,
                },
                "root",
                Utc::now(),
            )
            .await
            .unwrap();
        member
    }

    pub(crate) fn remote_id(&self, name: &str) -> String {
        self.fake.user(name).unwrap()["Id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    pub(crate) async fn reconcile(&self) -> starr_core::jobs::Report {
        self.jobs
            .run(JobKind::AccountsReconcile, ctx(&self.db))
            .await
            .unwrap()
    }

    pub(crate) async fn fingerprint(&self) -> String {
        self.db
            .account_plan(self.id)
            .await
            .unwrap()
            .unwrap()
            .fingerprint
    }

    /// Plan, then confirm the stored plan as an admin would.
    pub(crate) async fn confirm(&self) {
        self.reconcile().await;
        let fp = self.fingerprint().await;
        let outcome = self
            .db
            .confirm_account_plan(self.id, &fp, "root", Utc::now())
            .await
            .unwrap();
        assert!(
            matches!(outcome, Some(ConfirmOutcome::Confirmed { .. })),
            "{outcome:?}"
        );
    }

    pub(crate) async fn audit_actions(&self) -> Vec<(String, Value)> {
        self.db
            .recent_audit(100)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.action.starts_with("account."))
            .rev()
            .map(|e| {
                (
                    e.action,
                    serde_json::from_str(e.details.as_deref().unwrap_or("null")).unwrap(),
                )
            })
            .collect()
    }
}

#[tokio::test]
async fn with_writes_off_or_paused_nothing_but_get_is_ever_sent() {
    let s = setup(FakeFlavor::Jellyfin, 0).await;
    let friend = s.linked("friend", "friend").await;
    s.db.suspend_member(friend).await.unwrap();
    s.linked("alex", "Alex").await;
    // Plan only by default (upgrade and every new instance).
    for _ in 0..2 {
        let report = s.reconcile().await;
        assert!(
            matches!(report.expectation(), Expectation::Met(_)),
            "{report:?}"
        );
    }
    let stored = s.db.account_plan(s.id).await.unwrap().unwrap();
    let plan: starr_core::provisioning::Plan = serde_json::from_str(&stored.plan).unwrap();
    assert_eq!(
        plan.executable_steps().0,
        2,
        "a disable and a library change are planned"
    );
    assert_eq!(s.fake.writes().await, vec![]);

    // Live, then paused: plans go on, writes do not.
    s.confirm().await;
    s.db.pause_account_writes(s.id, "root", Utc::now())
        .await
        .unwrap();
    s.reconcile().await;
    s.jobs.run(JobKind::AccountsScan, ctx(&s.db)).await.unwrap();
    s.reconcile().await;
    assert_eq!(s.fake.writes().await, vec![], "paused writes send nothing");
}

#[tokio::test]
async fn going_live_executes_exactly_the_confirmed_plan_and_reads_back() {
    let s = setup(FakeFlavor::Jellyfin, 0).await;
    s.linked("friend", "friend").await;
    let friend_id = s.remote_id("friend");
    let before = s.fake.user("friend").unwrap()["Policy"].clone();
    s.confirm().await;
    let report = s.reconcile().await;
    assert!(
        matches!(report.expectation(), Expectation::Met(_)),
        "{report:?}"
    );
    assert_eq!(report.counter("applied"), Some(1));
    // One full-policy write: every library → only Movies.
    let mut body = before;
    body["EnableAllFolders"] = json!(false);
    body["EnabledFolders"] = json!([JF_MOVIES]);
    assert_eq!(
        s.fake.writes().await,
        vec![Write {
            method: "POST".into(),
            target: format!("/Users/{friend_id}/Policy"),
            body,
        }]
    );
    // Read back into the inventory; the next plan has nothing left.
    let row =
        s.db.remote_accounts(s.id)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.remote_id == friend_id)
            .unwrap();
    assert_eq!(row.libraries, Some(vec![JF_MOVIES.to_owned()]));
    let stored = s.db.account_plan(s.id).await.unwrap().unwrap();
    let plan: starr_core::provisioning::Plan = serde_json::from_str(&stored.plan).unwrap();
    assert_eq!(plan.executable_steps().0, 0);
    let writes = s.db.account_writes(s.id).await.unwrap().unwrap();
    assert!(writes.last_live_run_at.is_some());
    assert_eq!(writes.approval, None, "the approval is used up");
    // Audited with the outcome, no secret.
    let audit = s.audit_actions().await;
    let (action, details) = audit.last().unwrap();
    assert_eq!(action, "account.libraries");
    assert_eq!(details["result"], json!("ok"));
    assert_eq!(details["remoteId"], json!(friend_id));
    assert_eq!(details["libraries"], json!([JF_MOVIES]));

    // A retry (same job again, fresh live run) writes nothing more.
    s.reconcile().await;
    assert_eq!(s.fake.writes().await.len(), 1);
}

#[tokio::test]
async fn a_suspension_disables_within_the_hour_without_asking() {
    let s = setup(FakeFlavor::Emby, 0).await;
    let friend = s.linked("friend", "friend").await;
    s.confirm().await;
    s.reconcile().await;
    let after_live = s.fake.writes().await.len();
    s.db.suspend_member(friend).await.unwrap();
    s.reconcile().await;
    let writes = s.fake.writes().await;
    assert_eq!(writes.len(), after_live + 1);
    let last = writes.last().unwrap();
    assert_eq!(
        last.target,
        format!("/Users/{}/Policy", s.remote_id("friend"))
    );
    assert_eq!(last.body["IsDisabled"], json!(true));
    // Emby: the full policy went out, so no field was reset.
    assert_eq!(last.body["EnabledFolders"], json!([EMBY_MOVIES]));
    assert_eq!(last.body["EnableRemoteAccess"], json!(true));
    let (action, details) = s.audit_actions().await.pop().unwrap();
    assert_eq!(action, "account.disable");
    assert_eq!(details["reason"], json!("suspended"));
}

#[tokio::test]
async fn after_an_hour_without_a_live_run_the_next_write_waits() {
    let s = setup(FakeFlavor::Jellyfin, 0).await;
    let friend = s.linked("friend", "friend").await;
    s.confirm().await;
    s.reconcile().await;
    let after_live = s.fake.writes().await.len();
    // The last complete live run was two hours ago (Starr was down).
    s.db.record_live_run(s.id, "-", Utc::now() - TimeDelta::hours(2))
        .await
        .unwrap();
    s.db.suspend_member(friend).await.unwrap();
    let report = s.reconcile().await;
    assert_eq!(report.counter("held"), Some(1));
    assert_eq!(
        s.fake.writes().await.len(),
        after_live,
        "held: nothing written"
    );
    // The admin approves exactly this plan: it runs.
    s.confirm().await;
    s.reconcile().await;
    assert_eq!(s.fake.writes().await.len(), after_live + 1);
}

#[tokio::test]
async fn more_destructive_steps_than_the_cap_wait_for_approval() {
    let s = setup(FakeFlavor::Jellyfin, 6).await;
    let mut members = Vec::new();
    for i in 1..=6 {
        members.push(s.linked(&format!("m{i}"), &format!("friend-{i}")).await);
    }
    s.confirm().await;
    s.reconcile().await;
    let after_live = s.fake.writes().await.len();
    for m in &members {
        s.db.suspend_member(*m).await.unwrap();
    }
    let report = s.reconcile().await;
    assert_eq!(report.counter("held"), Some(1));
    assert_eq!(report.counter("destructive"), Some(6));
    assert_eq!(
        s.fake.writes().await.len(),
        after_live,
        "over the cap: nothing written"
    );
    s.confirm().await;
    s.reconcile().await;
    let writes = s.fake.writes().await;
    assert_eq!(writes.len(), after_live + 6);
    assert!(
        writes[after_live..]
            .iter()
            .all(|w| w.body["IsDisabled"] == json!(true))
    );
}

#[tokio::test]
async fn administrators_ignored_and_locked_accounts_are_never_written() {
    let s = setup(FakeFlavor::Jellyfin, 0).await;
    let locked = s.linked("locked", "locked-friend").await;
    let ignored = s.linked("restricted", "restricted").await;
    s.db.set_remote_account_ignored(s.id, &s.remote_id("restricted"), Some("root"), Utc::now())
        .await
        .unwrap();
    // An administrator can never be linked in the first place.
    let refused =
        s.db.decide_service_account(
            ignored,
            s.id,
            DecisionChange::Link {
                remote_id: &s.remote_id("admin"),
            },
            "root",
            Utc::now(),
        )
        .await
        .unwrap();
    assert!(matches!(
        refused,
        starr_db::service_accounts::DecideOutcome::Refused(_)
    ));
    s.confirm().await;
    s.reconcile().await;
    s.db.suspend_member(ignored).await.unwrap();
    s.reconcile().await;
    // The ignored account got nothing. The locked one only had its library
    // list narrowed: the lock (disabled, failed-login counter) stays as
    // the service set it (D10).
    let writes = s.fake.writes().await;
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(
        writes[0].target,
        format!("/Users/{}/Policy", s.remote_id("locked-friend"))
    );
    assert_eq!(writes[0].body["IsDisabled"], json!(true));
    assert_eq!(writes[0].body["InvalidLoginAttemptCount"], json!(4));
    assert_eq!(writes[0].body["EnabledFolders"], json!([JF_MOVIES]));
    // The locked account is reported, never re-enabled.
    let stored = s.db.account_plan(s.id).await.unwrap().unwrap();
    let plan: starr_core::provisioning::Plan = serde_json::from_str(&stored.plan).unwrap();
    let entry = plan.entries.iter().find(|e| e.member_id == locked).unwrap();
    assert_eq!(
        entry.state,
        starr_core::provisioning::State::LockedByService
    );
    // No request at all touched the administrators.
    let admin = s.remote_id("admin");
    let touched: Vec<_> = s
        .fake
        .server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path().contains(&admin))
        .collect();
    assert!(touched.is_empty());
}

#[tokio::test]
async fn a_failed_write_lands_in_needs_you_and_clears_on_success() {
    let s = setup(FakeFlavor::Jellyfin, 0).await;
    s.linked("friend", "friend").await;
    s.confirm().await;
    s.fake.fail_policy_writes();
    let report = s.reconcile().await;
    assert!(
        matches!(report.expectation(), Expectation::Unmet(_)),
        "{report:?}"
    );
    assert_eq!(report.counter("failed"), Some(1));
    let failures = s.db.account_write_failures().await.unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].username, "friend");
    assert_eq!(failures[0].code, "transient");
    let (action, details) = s.audit_actions().await.pop().unwrap();
    assert_eq!(action, "account.libraries");
    assert_eq!(details["result"], json!("failed"));
    // Service trouble is no complete live run; the approval stays for the
    // retry of the same plan.
    let writes = s.db.account_writes(s.id).await.unwrap().unwrap();
    assert_eq!(writes.last_live_run_at, None);
    assert!(writes.approval.is_some());

    let s2 = &s;
    // The service is back: the retry succeeds and the failure clears.
    let recovered = MediaServerFake::start(
        FakeFlavor::Jellyfin,
        AuthScheme::MediaBrowserToken,
        KEY,
        Recorded::from_dir(&format!(
            "{}/../adapters/jellyfin/fixtures/12.3.0/accounts",
            env!("CARGO_MANIFEST_DIR")
        )),
        "friend",
    )
    .await;
    s2.db
        .update_service(
            s2.id,
            &starr_db::services::ServiceUpdate {
                base_url: Some(&recovered.server.uri()),
                ..Default::default()
            },
            Utc::now(),
        )
        .await
        .unwrap();
    s2.reconcile().await;
    assert_eq!(recovered.writes().await.len(), 1);
    assert_eq!(s2.db.account_write_failures().await.unwrap(), vec![]);
}

#[tokio::test]
async fn an_incomplete_listing_is_never_executed_even_when_approved() {
    let s = setup(FakeFlavor::Jellyfin, 0).await;
    s.linked("friend", "friend").await;
    s.confirm().await;
    // An unreadable account makes the next listing incomplete.
    s.fake.edit_user("Alex", |u| {
        u.as_object_mut().unwrap().remove("Policy");
    });
    s.jobs.run(JobKind::AccountsScan, ctx(&s.db)).await.unwrap();
    let report = s.reconcile().await;
    assert_eq!(report.counter("incomplete"), Some(1));
    assert_eq!(s.fake.writes().await, vec![]);
}
#[tokio::test]
async fn a_pause_during_a_run_stops_the_remaining_members() {
    let s = setup(FakeFlavor::Jellyfin, 0).await;
    let friend = s.linked("friend", "friend").await;
    s.confirm().await;
    s.reconcile().await;
    let before = s.fake.writes().await.len();
    // A plan with a step for friend; the admin pauses as the run starts:
    // nothing is written and the run is not a complete live run.
    let plan = starr_core::provisioning::Plan {
        complete: true,
        entries: vec![starr_core::provisioning::PlanEntry {
            member_id: friend,
            state: starr_core::provisioning::State::Managed,
            remote_id: Some(s.remote_id("friend")),
            steps: vec![starr_core::provisioning::Step::new(
                starr_core::provisioning::Action::Disable,
                starr_core::provisioning::Reason::Suspended,
            )],
        }],
    };
    s.db.pause_account_writes(s.id, "root", Utc::now())
        .await
        .unwrap();
    let done = crate::account_writes::execute(
        &s.db,
        s.jobs.openers(),
        &CancellationToken::new(),
        s.id,
        "jellyfin",
        &plan,
        |_| Some("friend".to_owned()),
    )
    .await
    .unwrap();
    let Ok(done) = done else {
        panic!("not executed")
    };
    assert!(done.interrupted);
    assert_eq!((done.applied, done.skipped), (0, 1));
    assert_eq!(s.fake.writes().await.len(), before);
}
#[tokio::test]
async fn a_failure_with_nothing_left_to_retry_leaves_needs_you() {
    let s = setup(FakeFlavor::Jellyfin, 0).await;
    s.linked("friend", "friend").await;
    s.confirm().await;
    s.fake.fail_policy_writes();
    s.reconcile().await;
    assert_eq!(s.db.account_write_failures().await.unwrap().len(), 1);
    // A retry is still planned: the failure stays.
    s.reconcile().await;
    assert_eq!(s.db.account_write_failures().await.unwrap().len(), 1);
    // The admin marks the account ignored: no step is left, so it settles.
    s.db.set_remote_account_ignored(s.id, &s.remote_id("friend"), Some("root"), Utc::now())
        .await
        .unwrap();
    s.reconcile().await;
    assert_eq!(s.db.account_write_failures().await.unwrap(), vec![]);
}
