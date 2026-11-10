//! Navidrome accounts end to end in the worker (spec 0006 slice 7, spec
//! 0005 D9) against the stateful local fake seeded from the **derived**
//! fixtures (never a real Navidrome): scan, plan, go live, create from a
//! handoff, suspension scrambles the password, reactivation waits for the
//! member's password, which unlocks the account. Admin accounts are never
//! written; no password is stored anywhere but in the request carrying it.
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use secrecy::SecretString;
use serde_json::json;
use starr_adapter_kit::LoginCredential;
use starr_adapter_kit::contract::navidrome::{Derived, NavidromeFake};
use starr_core::jobs::{JobKind, Report};
use starr_core::members::{Category, Entitlement, RequestMode};
use starr_core::provisioning::{Action, PasswordUse, Plan, Reason};
use starr_db::Database;
use starr_db::account_writes::ConfirmOutcome;
use starr_db::members::NewMember;
use starr_db::service_accounts::{DecisionChange, ServiceAccountRow};
use starr_db::services::{NewServiceInstance, StoredCredential};

use crate::account_writes_tests::ctx;
use crate::secrets::{CredentialKey, seal_password_handoff};
use crate::{JobHandler, SystemJobs};

const ADMIN_PASSWORD: &str = "starr-admin-0123";
const MIA_PASSWORD: &str = "Mia hört 🎵 \"laut\" \\ %20";
const ALEX_PASSWORD: &str = "Alex' neues Passwort";
const OPERATOR: &str = "0Fq7Hc2Vd8Jk5Lp3Mn9Rb1";
const STARR: &str = "6aB3kQ9zXw1Lm4Np7Rs2Tu";
const ALEX: &str = "3Tz5Wq8Ey1Ru4Io7Pa2Sd6";

struct Nd {
    db: Database,
    jobs: SystemJobs,
    fake: NavidromeFake,
    id: i64,
}

async fn setup() -> Nd {
    let dir = format!(
        "{}/../adapters/navidrome/fixtures/0.64.2/derived",
        env!("CARGO_MANIFEST_DIR")
    );
    let fake = NavidromeFake::start(
        Derived::from_dir(&dir),
        &[("starr", ADMIN_PASSWORD), ("Alex", "alex-old-password")],
    )
    .await;
    let db = Database::open_in_memory().await.unwrap();
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let sealer = starr_secrets::CredentialSealer::parse(jobs.credentials.recipient()).unwrap();
    let login = LoginCredential::encode("starr", &SecretString::from(ADMIN_PASSWORD.to_owned()));
    let (recipient, ciphertext) = sealer.seal(&login).unwrap().into_parts();
    let id = db
        .create_service(
            &NewServiceInstance {
                adapter_type: "navidrome",
                name: "Navidrome",
                base_url: &fake.server.uri(),
                public_url: Some("https://music.example.org"),
                credential: &StoredCredential {
                    ciphertext,
                    recipient,
                    class: starr_core::adapters::SecretClass::ServiceCredential,
                },
            },
            Utc::now(),
        )
        .await
        .unwrap()
        .id;
    db.add_role(
        &starr_db::categories::NewRole {
            category: Category::Music,
            kind: starr_core::categories::RoleKind::Consume,
            service_instance_id: id,
            offered: &[starr_core::adapters::Capability::Libraries],
        },
        Utc::now(),
    )
    .await
    .unwrap();
    let role = db.list_roles(None).await.unwrap().remove(0);
    db.set_consume_libraries(
        role.id,
        &starr_core::libraries::ConsumeSettings {
            libraries: vec![starr_core::libraries::MappedLibrary {
                id: "1".to_owned(),
                grant_id: "1".to_owned(),
            }],
        },
        Utc::now(),
    )
    .await
    .unwrap();
    db.set_category_enabled(Category::Music, true, Utc::now())
        .await
        .unwrap();
    let nd = Nd { db, jobs, fake, id };
    nd.run(JobKind::AccountsScan).await;
    nd
}

impl Nd {
    async fn run(&self, kind: JobKind) -> Report {
        self.jobs.run(kind, ctx(&self.db)).await.unwrap()
    }

    async fn member(&self, name: &str) -> i64 {
        let email = format!("{name}@example.org");
        self.db
            .create_member(
                &NewMember {
                    username: name,
                    email: &email,
                    display_name: name,
                    password_hash: Some("$argon2id$dummy"),
                },
                &[Entitlement {
                    category: Category::Music,
                    enabled: true,
                    request_mode: RequestMode::Approval,
                }],
                Utc::now(),
            )
            .await
            .unwrap()
    }

    async fn confirm(&self) {
        self.run(JobKind::AccountsReconcile).await;
        let fp = self
            .db
            .account_plan(self.id)
            .await
            .unwrap()
            .unwrap()
            .fingerprint;
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

    async fn row(&self, member: i64) -> ServiceAccountRow {
        self.db
            .member_service_accounts(member)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.service_instance_id == self.id)
            .unwrap()
    }

    /// Whether Starr locked the member's account there.
    async fn locked(&self, member: i64) -> bool {
        let rows = self.db.member_service_accounts(member).await.unwrap();
        let Some(remote_id) = rows
            .into_iter()
            .find(|r| r.service_instance_id == self.id)
            .and_then(|r| r.remote_id)
        else {
            return false;
        };
        self.db
            .remote_accounts(self.id)
            .await
            .unwrap()
            .into_iter()
            .any(|r| r.remote_id == remote_id && r.locked_by_starr)
    }

    async fn plan(&self) -> Plan {
        let stored = self.db.account_plan(self.id).await.unwrap().unwrap();
        serde_json::from_str(&stored.plan).unwrap()
    }

    async fn steps(&self, member: i64) -> Vec<(Action, Reason)> {
        self.plan()
            .await
            .entries
            .into_iter()
            .find(|e| e.member_id == member)
            .unwrap()
            .steps
            .into_iter()
            .map(|s| (s.action, s.reason))
            .collect()
    }

    /// Everything Starr stored that could hold a secret.
    async fn stored_text(&self) -> String {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT coalesce(details, '') FROM audit_log
             UNION ALL SELECT plan FROM account_plan
             UNION ALL SELECT coalesce(last_error_detail, '') FROM service_account
             UNION ALL SELECT coalesce(payload, '') FROM notification
             UNION ALL SELECT coalesce(report, '') FROM run",
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

/// The whole story with two members (lesson 4): mia is created from her
/// handoff, alex's existing account is adopted. Suspension scrambles both
/// once; reactivation writes nothing until each member's password comes;
/// alex's password unlocks only alex's account.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one story
async fn suspension_scrambles_and_only_the_member_password_unlocks() {
    let nd = setup().await;
    let mia = nd.member("mia").await;
    nd.db
        .initial_service_accounts(mia, &[nd.id], &[], Utc::now())
        .await
        .unwrap();
    let alex = nd.member("alex").await;
    nd.db
        .decide_service_account(
            alex,
            nd.id,
            DecisionChange::Link { remote_id: ALEX },
            "root",
            Utc::now(),
        )
        .await
        .unwrap();
    nd.confirm().await;
    nd.run(JobKind::AccountsReconcile).await;
    assert_eq!(
        nd.fake.writes().await,
        vec![],
        "nothing to do without a password"
    );

    // Mia's account is created from her handoff with only Music Library.
    nd.handoff(mia, MIA_PASSWORD).await;
    nd.run(JobKind::AccountsPassword).await;
    assert!(nd.fake.login_works("mia", MIA_PASSWORD));
    let mia_id = nd.row(mia).await.remote_id.unwrap();
    assert_eq!(nd.fake.grants(&mia_id), vec![1]);
    let created = nd.fake.writes().await.len();
    assert_eq!(created, 3, "create, libraries, password");

    // Suspension: one scramble each, recorded as Starr's lock.
    for m in [mia, alex] {
        nd.db.suspend_member(m).await.unwrap();
    }
    nd.run(JobKind::AccountsReconcile).await;
    let writes = nd.fake.writes().await;
    let scrambles: Vec<_> = writes[created..].iter().collect();
    assert_eq!(scrambles.len(), 2, "{scrambles:?}");
    for w in &scrambles {
        assert_eq!(w.method, "PUT");
        let password = w.body["password"].as_str().unwrap();
        assert_eq!(password.len(), 64);
    }
    assert!(!nd.fake.login_works("mia", MIA_PASSWORD));
    assert!(!nd.fake.login_works("alex", "alex-old-password"));
    assert!(nd.locked(mia).await);
    assert!(nd.locked(alex).await);
    // Admins are never written (Starr's own and the operator's).
    assert!(
        writes
            .iter()
            .all(|w| !w.target.contains(STARR) && !w.target.contains(OPERATOR))
    );
    assert!(nd.fake.login_works("starr", ADMIN_PASSWORD));

    // Locked: the next runs scramble nothing again.
    nd.run(JobKind::AccountsScan).await;
    nd.run(JobKind::AccountsReconcile).await;
    assert_eq!(nd.fake.writes().await.len(), created + 2);

    // Reactivation: nothing to enable; each waits for its password.
    for m in [mia, alex] {
        nd.db.reactivate_member(m).await.unwrap();
    }
    nd.run(JobKind::AccountsReconcile).await;
    assert_eq!(
        nd.fake.writes().await.len(),
        created + 2,
        "no enable exists"
    );
    for m in [mia, alex] {
        assert_eq!(
            nd.steps(m).await,
            vec![(Action::SetPassword, Reason::LockedByStarr)]
        );
    }
    assert!(nd.db.password_handoff_wanted(alex).await.unwrap());

    // Alex's password unlocks alex only (the adopted account now uses the
    // Starr password); mia stays locked.
    nd.handoff(alex, ALEX_PASSWORD).await;
    nd.run(JobKind::AccountsPassword).await;
    assert!(nd.fake.login_works("alex", ALEX_PASSWORD));
    assert!(!nd.fake.login_works("mia", MIA_PASSWORD));
    let alex_row = nd.row(alex).await;
    assert!(!nd.locked(alex).await);
    assert_eq!(alex_row.password, Some(PasswordUse::Starr));
    assert!(nd.locked(mia).await);
    nd.run(JobKind::AccountsReconcile).await;
    assert_eq!(nd.steps(alex).await, vec![]);

    // Audited without a value; no password is stored anywhere.
    let audit: Vec<(String, serde_json::Value)> = nd
        .db
        .recent_audit(100)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.action == "account.disable")
        .map(|e| {
            (
                e.action,
                serde_json::from_str(e.details.as_deref().unwrap_or("null")).unwrap(),
            )
        })
        .collect();
    assert_eq!(audit.len(), 2);
    assert!(
        audit
            .iter()
            .all(|(_, d)| d["lockedByStarr"] == json!(true) && d["enabled"] == json!(false))
    );
    let stored = nd.stored_text().await;
    for secret in [MIA_PASSWORD, ALEX_PASSWORD, ADMIN_PASSWORD] {
        assert!(!stored.contains(secret), "{secret:?} stored");
    }
    for w in nd.fake.writes().await {
        if let Some(p) = w.body["password"].as_str() {
            assert!(!stored.contains(p), "a scrambled password was stored");
        }
    }
}

/// A create that stopped after the scrambled account existed: owned and
/// locked by Starr, never scrambled again by a plan, unlocked by the
/// member's next password.
#[tokio::test]
async fn an_unfinished_create_stays_locked_until_the_password_lands() {
    let nd = setup().await;
    let mia = nd.member("mia").await;
    let sam = nd.member("sam2").await;
    for m in [mia, sam] {
        nd.db
            .initial_service_accounts(m, &[nd.id], &[], Utc::now())
            .await
            .unwrap();
    }
    nd.confirm().await;
    nd.fake.fail_library_writes();
    nd.handoff(mia, MIA_PASSWORD).await;
    nd.run(JobKind::AccountsPassword).await;
    let row = nd.row(mia).await;
    assert!(row.remote_id.is_some() && nd.locked(mia).await);
    assert!(!nd.fake.login_works("mia", MIA_PASSWORD));
    // Sam had no handoff: nothing created for sam.
    assert!(nd.fake.user("sam2").is_none());
    nd.run(JobKind::AccountsScan).await;
    nd.run(JobKind::AccountsReconcile).await;
    let steps = nd.steps(mia).await;
    assert!(
        steps.iter().all(|(a, _)| *a != Action::Disable),
        "already locked: {steps:?}"
    );
    assert_eq!(
        steps.last(),
        Some(&(Action::SetPassword, Reason::LockedByStarr))
    );
}

/// Plan only (the default): a suspension is planned, nothing is written.
#[tokio::test]
async fn with_writes_off_navidrome_gets_no_write() {
    let nd = setup().await;
    let alex = nd.member("alex").await;
    nd.db
        .decide_service_account(
            alex,
            nd.id,
            DecisionChange::Link { remote_id: ALEX },
            "root",
            Utc::now(),
        )
        .await
        .unwrap();
    nd.db.suspend_member(alex).await.unwrap();
    nd.run(JobKind::AccountsReconcile).await;
    assert_eq!(
        nd.steps(alex).await,
        vec![(Action::Disable, Reason::Suspended)]
    );
    assert_eq!(nd.fake.writes().await, vec![]);
    assert!(!nd.locked(alex).await);
}
/// D16 without a flag (two members): at the deadline alex's adopted
/// account is scrambled once and then released, so the member no longer
/// owns accounts and can be deleted; mia's account Starr created is
/// deleted.
#[tokio::test]
async fn at_the_deadline_adopted_accounts_are_locked_and_released() {
    use chrono::TimeDelta;
    let nd = setup().await;
    let alex = nd.member("alex").await;
    nd.db
        .decide_service_account(
            alex,
            nd.id,
            DecisionChange::Link { remote_id: ALEX },
            "root",
            Utc::now(),
        )
        .await
        .unwrap();
    let mia = nd.member("mia").await;
    nd.db
        .initial_service_accounts(mia, &[nd.id], &[], Utc::now())
        .await
        .unwrap();
    nd.confirm().await;
    nd.run(JobKind::AccountsReconcile).await;
    nd.handoff(mia, MIA_PASSWORD).await;
    nd.run(JobKind::AccountsPassword).await;
    assert!(nd.fake.login_works("mia", MIA_PASSWORD));
    let now = Utc::now();
    for m in [alex, mia] {
        assert!(
            nd.db
                .schedule_member_deletion(
                    m,
                    "root",
                    now - TimeDelta::minutes(1),
                    now - TimeDelta::days(30)
                )
                .await
                .unwrap()
        );
    }
    nd.run(JobKind::AccountsReconcile).await;
    // Released by the replan after the scramble.
    nd.run(JobKind::AccountsReconcile).await;
    assert!(!nd.fake.login_works("alex", "alex-old-password"));
    assert!(nd.fake.user("Alex").is_some(), "adopted: never deleted");
    assert!(nd.fake.user("mia").is_none(), "created by Starr: deleted");
    assert!(!nd.db.member_owns_accounts(alex).await.unwrap());
    assert!(!nd.db.member_owns_accounts(mia).await.unwrap());
    // Exactly one scramble of alex's account.
    let scrambles = nd
        .fake
        .writes()
        .await
        .iter()
        .filter(|w| w.target == format!("/api/user/{ALEX}"))
        .count();
    assert_eq!(scrambles, 1);
}

/// The lock is recorded before the scramble is sent: a scramble whose
/// read-back fails still leaves the account locked, so the reactivated
/// member is asked for their password instead of shown "ready".
#[tokio::test]
async fn a_failed_read_back_after_the_scramble_still_leaves_the_lock() {
    let nd = setup().await;
    let alex = nd.member("alex").await;
    let sam = nd.member("sam").await;
    nd.db
        .decide_service_account(
            alex,
            nd.id,
            DecisionChange::Link { remote_id: ALEX },
            "root",
            Utc::now(),
        )
        .await
        .unwrap();
    nd.confirm().await;
    nd.run(JobKind::AccountsReconcile).await;
    nd.db.suspend_member(alex).await.unwrap();
    nd.fake.break_user_writes();
    nd.run(JobKind::AccountsReconcile).await;
    assert!(nd.locked(alex).await);
    assert!(!nd.locked(sam).await);
    nd.db.reactivate_member(alex).await.unwrap();
    nd.run(JobKind::AccountsReconcile).await;
    assert_eq!(
        nd.steps(alex).await,
        vec![(Action::SetPassword, Reason::LockedByStarr)]
    );
}
