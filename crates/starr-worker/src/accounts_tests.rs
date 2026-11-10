#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use secrecy::SecretString;
use serde_json::{Value, json};
use starr_core::jobs::{Expectation, ItemOutcome, JobKind};
use starr_db::services::{NewServiceInstance, StoredCredential};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;
use crate::secrets::CredentialKey;
use crate::{JobContext, JobHandler, SystemJobs};

const KEY: &str = "accounts-key-0123456789";

fn fixture(adapter: &str, version: &str, name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/{adapter}/fixtures/{version}/accounts/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// Serves `GET /Users` from a body the test can change between scans.
#[derive(Clone)]
struct UsersBody(Arc<Mutex<String>>);

impl Respond for UsersBody {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_string(self.0.lock().unwrap().clone())
    }
}

struct Fake {
    server: MockServer,
    users: UsersBody,
}

impl Fake {
    /// Replace the listing with the recorded one minus accounts named in
    /// `drop`; `break_one` strips the policy of one remaining entry (an
    /// unreadable account, so the listing is incomplete).
    fn set_users(&self, all: &str, drop: &[&str], break_one: Option<&str>) {
        let mut users: Vec<Value> = serde_json::from_str(all).unwrap();
        users.retain(|u| !drop.contains(&u["Name"].as_str().unwrap()));
        for u in &mut users {
            if Some(u["Name"].as_str().unwrap()) == break_one {
                u.as_object_mut().unwrap().remove("Policy");
            }
        }
        *self.users.0.lock().unwrap() = Value::Array(users).to_string();
    }

    /// Every request the fake received, as `METHOD path`.
    async fn non_get_requests(&self) -> Vec<String> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method.as_str() != "GET")
            .map(|r| format!("{} {}", r.method, r.url.path()))
            .collect()
    }
}

/// A fake service from recorded fixtures that answers only `GET` with the
/// right key. Anything else is recorded, so tests can assert that a scan
/// sent no write at all.
async fn fake(adapter: &str, version: &str, auth: (&str, String)) -> Fake {
    fake_with(adapter, version, auth, true).await
}

/// [`fake`], optionally without the library listing (answered 401 then).
async fn fake_with(adapter: &str, version: &str, auth: (&str, String), folders: bool) -> Fake {
    let server = MockServer::start().await;
    let users = UsersBody(Arc::new(Mutex::new(fixture(
        adapter,
        version,
        "users.json",
    ))));
    Mock::given(method("GET"))
        .and(path("/System/Info"))
        .and(header(auth.0, auth.1.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture(
            adapter,
            version,
            "system_info.json",
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Users"))
        .and(header(auth.0, auth.1.as_str()))
        .respond_with(users.clone())
        .mount(&server)
        .await;
    if folders {
        Mock::given(method("GET"))
            .and(path("/Library/VirtualFolders"))
            .and(header(auth.0, auth.1.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_string(fixture(
                adapter,
                version,
                "virtual_folders.json",
            )))
            .mount(&server)
            .await;
    }
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Fake { server, users }
}

async fn fake_jellyfin() -> Fake {
    fake(
        "jellyfin",
        "12.3.0",
        ("authorization", format!("MediaBrowser Token=\"{KEY}\"")),
    )
    .await
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

async fn add(db: &Database, jobs: &SystemJobs, adapter_type: &str, url: &str, key: &str) -> i64 {
    db.create_service(
        &NewServiceInstance {
            adapter_type,
            name: &format!("{adapter_type}-{url}"),
            base_url: url,
            public_url: None,
            credential: &seal(&jobs.credentials, key),
        },
        Utc::now(),
    )
    .await
    .unwrap()
    .id
}

async fn watch_in(db: &Database, id: i64) {
    db.add_role(
        &starr_db::categories::NewRole {
            category: starr_core::members::Category::Movies,
            kind: starr_core::categories::RoleKind::Consume,
            service_instance_id: id,
            offered: &[starr_core::adapters::Capability::Libraries],
        },
        Utc::now(),
    )
    .await
    .unwrap();
}

fn ctx(db: &Database) -> JobContext {
    JobContext {
        db: db.clone(),
        payload: json!({}),
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn a_scan_stores_every_account_and_sends_no_write() {
    let db = Database::open_in_memory().await.unwrap();
    let fake = fake_jellyfin().await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = add(&db, &jobs, "jellyfin", &fake.server.uri(), KEY).await;
    assert!(!jobs.has_work(JobKind::AccountsScan, &db).await.unwrap());
    watch_in(&db, id).await;
    assert!(jobs.has_work(JobKind::AccountsScan, &db).await.unwrap());

    let report = jobs.run(JobKind::AccountsScan, ctx(&db)).await.unwrap();
    assert!(
        matches!(report.expectation(), Expectation::Met(_)),
        "{report:?}"
    );
    assert_eq!(report.counter("listed"), Some(7));
    assert_eq!(report.counter("privileged"), Some(2));
    assert_eq!(report.counter("appeared"), Some(7));

    let rows = db.remote_accounts(id).await.unwrap();
    let names: Vec<_> = rows.iter().map(|r| r.username.as_str()).collect();
    assert_eq!(
        names,
        [
            "admin",
            "Alex",
            "disabled-friend",
            "friend",
            "locked-friend",
            "restricted",
            "second-admin"
        ]
    );
    let locked = &rows[4];
    assert!(locked.locked_by_service && !locked.enabled);
    assert_eq!(
        rows[5].libraries,
        Some(vec!["f137a2dd21bbc1b99aa5c0f6bf02a805".to_owned()])
    );
    let state = db.account_scan_state(id).await.unwrap().unwrap();
    assert_eq!((state.complete, state.error), (Some(true), None));

    // The libraries were listed too (slice 2).
    assert_eq!(report.counter("libraries"), Some(2));
    let libraries = db.service_libraries(id).await.unwrap();
    let got: Vec<_> = libraries
        .iter()
        .map(|l| {
            (
                l.library.name.as_str(),
                l.library.grant_id.as_deref().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("Movies", "f137a2dd21bbc1b99aa5c0f6bf02a805"),
            ("Shows", "a656b907eb3a73532e40e44b968d0225")
        ]
    );
    let listed = db.library_listing_state(id).await.unwrap().unwrap();
    assert_eq!((listed.complete, listed.error), (Some(true), None));

    // A second scan changes nothing, and no scan ever wrote anything.
    jobs.run(JobKind::AccountsScan, ctx(&db)).await.unwrap();
    assert_eq!(db.remote_accounts(id).await.unwrap().len(), 7);
    assert_eq!(fake.non_get_requests().await, Vec::<String>::new());
}

#[tokio::test]
async fn emby_accounts_are_scanned_without_writes() {
    let db = Database::open_in_memory().await.unwrap();
    let fake = fake("emby", "4.10.1.0", ("x-emby-token", KEY.to_owned())).await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = add(&db, &jobs, "emby", &fake.server.uri(), KEY).await;
    watch_in(&db, id).await;
    let report = jobs.run(JobKind::AccountsScan, ctx(&db)).await.unwrap();
    assert!(
        matches!(report.expectation(), Expectation::Met(_)),
        "{report:?}"
    );
    let rows = db.remote_accounts(id).await.unwrap();
    assert_eq!(rows.len(), 7);
    assert_eq!(rows.iter().filter(|r| r.privileged).count(), 2);
    // Emby: item queries take the ItemId, grants the Guid; both are kept.
    let libraries = db.service_libraries(id).await.unwrap();
    let got: Vec<_> = libraries
        .iter()
        .map(|l| {
            (
                l.library.id.as_str(),
                l.library.grant_id.as_deref().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("3", "c26fd8b5c1644eaaac60078a62ba4d00"),
            ("5", "4032dc584f6a4f569ae151647f5b31dd")
        ]
    );
    assert_eq!(fake.non_get_requests().await, Vec::<String>::new());
}

#[tokio::test]
async fn accounts_go_only_after_two_complete_scans() {
    let db = Database::open_in_memory().await.unwrap();
    let fake = fake_jellyfin().await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = add(&db, &jobs, "jellyfin", &fake.server.uri(), KEY).await;
    watch_in(&db, id).await;
    let all = fixture("jellyfin", "12.3.0", "users.json");
    jobs.run(JobKind::AccountsScan, ctx(&db)).await.unwrap();

    let gone_since = |db: Database| async move {
        db.remote_accounts(id)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.username == "friend")
            .map(|r| (r.missing_runs, r.gone_since.is_some()))
    };
    // Incomplete listings without "friend" never count.
    fake.set_users(&all, &["friend"], Some("Alex"));
    for _ in 0..3 {
        let report = jobs.run(JobKind::AccountsScan, ctx(&db)).await.unwrap();
        assert!(
            matches!(report.expectation(), Expectation::Unmet(_)),
            "{report:?}"
        );
        assert_eq!(report.counter("incomplete"), Some(1));
    }
    assert_eq!(gone_since(db.clone()).await, Some((0, false)));
    let state = db.account_scan_state(id).await.unwrap().unwrap();
    assert_eq!(state.complete, Some(false));

    fake.set_users(&all, &["friend"], None);
    jobs.run(JobKind::AccountsScan, ctx(&db)).await.unwrap();
    assert_eq!(gone_since(db.clone()).await, Some((1, false)));
    let report = jobs.run(JobKind::AccountsScan, ctx(&db)).await.unwrap();
    assert_eq!(report.counter("gone"), Some(1));
    assert_eq!(gone_since(db.clone()).await, Some((2, true)));

    // Back again: present.
    fake.set_users(&all, &[], None);
    let report = jobs.run(JobKind::AccountsScan, ctx(&db)).await.unwrap();
    assert_eq!(report.counter("returned"), Some(1));
    assert_eq!(gone_since(db.clone()).await, Some((0, false)));
    assert_eq!(fake.non_get_requests().await, Vec::<String>::new());
}

#[tokio::test]
async fn a_failing_scan_is_recorded_and_keeps_the_inventory() {
    let db = Database::open_in_memory().await.unwrap();
    let fake = fake_jellyfin().await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = add(&db, &jobs, "jellyfin", &fake.server.uri(), KEY).await;
    watch_in(&db, id).await;
    jobs.run(JobKind::AccountsScan, ctx(&db)).await.unwrap();

    *fake.users.0.lock().unwrap() = "not json".to_owned();
    let report = jobs.run(JobKind::AccountsScan, ctx(&db)).await.unwrap();
    assert!(
        matches!(report.expectation(), Expectation::Unmet(_)),
        "{report:?}"
    );
    let item = &report.items()[0];
    assert_eq!(item.outcome, ItemOutcome::Failed);
    assert_eq!(item.code.as_deref(), Some("unknown"));
    let state = db.account_scan_state(id).await.unwrap().unwrap();
    assert_eq!(state.error.as_deref(), Some("unknown"));
    let rows = db.remote_accounts(id).await.unwrap();
    assert_eq!(rows.len(), 7);
    assert!(rows.iter().all(|r| r.missing_runs == 0));
    // The library listing worked, so it is not marked failed.
    let listed = db.library_listing_state(id).await.unwrap().unwrap();
    assert_eq!(listed.error, None);
}

#[tokio::test]
async fn an_unreachable_service_fails_both_listings_and_keeps_the_libraries() {
    let db = Database::open_in_memory().await.unwrap();
    let fake = fake_jellyfin().await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = add(&db, &jobs, "jellyfin", &fake.server.uri(), KEY).await;
    watch_in(&db, id).await;
    jobs.run(JobKind::AccountsScan, ctx(&db)).await.unwrap();
    // A wrong key: the probe fails, nothing is listed.
    db.update_service(
        id,
        &starr_db::services::ServiceUpdate {
            name: None,
            base_url: None,
            public_url: None,
            credential: Some(&seal(&jobs.credentials, "wrong-key")),
        },
        Utc::now(),
    )
    .await
    .unwrap();
    jobs.run(JobKind::AccountsScan, ctx(&db)).await.unwrap();
    let listed = db.library_listing_state(id).await.unwrap().unwrap();
    assert_eq!(listed.error.as_deref(), Some("credentials_invalid"));
    assert_eq!(db.service_libraries(id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn instances_without_accounts_are_not_scanned() {
    let db = Database::open_in_memory().await.unwrap();
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    add(&db, &jobs, "radarr", "http://radarr.invalid", KEY).await;
    // Not a consume instance, so no scan at all.
    assert!(!jobs.has_work(JobKind::AccountsScan, &db).await.unwrap());
    let report = jobs.run(JobKind::AccountsScan, ctx(&db)).await.unwrap();
    assert_eq!(report.expectation(), &Expectation::NotApplicable);
}

#[tokio::test]
async fn a_failing_library_listing_is_recorded_and_accounts_still_scan() {
    let db = Database::open_in_memory().await.unwrap();
    let fake = fake_with(
        "jellyfin",
        "12.3.0",
        ("authorization", format!("MediaBrowser Token=\"{KEY}\"")),
        false,
    )
    .await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = add(&db, &jobs, "jellyfin", &fake.server.uri(), KEY).await;
    watch_in(&db, id).await;
    jobs.run(JobKind::AccountsScan, ctx(&db)).await.unwrap();
    assert_eq!(db.remote_accounts(id).await.unwrap().len(), 7);
    let accounts = db.account_scan_state(id).await.unwrap().unwrap();
    assert_eq!(accounts.error, None);
    let listed = db.library_listing_state(id).await.unwrap().unwrap();
    assert_eq!(listed.error.as_deref(), Some("credentials_invalid"));
    assert_eq!(db.service_libraries(id).await.unwrap(), vec![]);
}
// --- accounts.reconcile (spec 0005 slice 3, plan only) ---

async fn member(db: &Database, name: &str, movies: bool) -> i64 {
    let email = format!("{name}@example.org");
    db.create_member(
        &starr_db::members::NewMember {
            username: name,
            email: &email,
            display_name: name,
            password_hash: None,
        },
        &[starr_core::members::Entitlement {
            category: starr_core::members::Category::Movies,
            enabled: movies,
            request_mode: starr_core::members::RequestMode::Approval,
        }],
        Utc::now(),
    )
    .await
    .unwrap()
}

/// Map the Jellyfin fixture's "Movies" library to movies.
async fn map_movies(db: &Database, instance: i64) {
    let role = db
        .list_roles(None)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.service_instance_id == instance)
        .unwrap();
    db.set_consume_libraries(
        role.id,
        &starr_core::libraries::ConsumeSettings {
            libraries: vec![starr_core::libraries::MappedLibrary {
                id: "f137a2dd21bbc1b99aa5c0f6bf02a805".into(),
                grant_id: "f137a2dd21bbc1b99aa5c0f6bf02a805".into(),
            }],
        },
        Utc::now(),
    )
    .await
    .unwrap();
    db.set_category_enabled(starr_core::members::Category::Movies, true, Utc::now())
        .await
        .unwrap();
}

fn stored_plan(db_plan: &starr_db::service_accounts::StoredPlan) -> starr_core::provisioning::Plan {
    serde_json::from_str(&db_plan.plan).unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one planning story
async fn reconcile_plans_from_the_scan_and_never_calls_the_service() {
    use starr_core::provisioning::{Action, Conflict, State};
    use starr_db::service_accounts::DecisionChange;

    let db = Database::open_in_memory().await.unwrap();
    let fake = fake_jellyfin().await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = add(&db, &jobs, "jellyfin", &fake.server.uri(), KEY).await;
    watch_in(&db, id).await;
    map_movies(&db, id).await;
    // Alex (equal name "Alex" there) stays undecided; friend is linked;
    // sam and "restricted" are decided "create" ("restricted" is taken).
    let alex = member(&db, "alex", true).await;
    let friend = member(&db, "friend", true).await;
    let sam = member(&db, "sam", true).await;
    let restricted = member(&db, "restricted", true).await;
    jobs.run(JobKind::AccountsScan, ctx(&db)).await.unwrap();
    let requests_after_scan = fake.server.received_requests().await.unwrap().len();

    let friend_remote = db
        .remote_accounts(id)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.username == "friend")
        .unwrap()
        .remote_id;
    db.decide_service_account(
        friend,
        id,
        DecisionChange::Link {
            remote_id: &friend_remote,
        },
        "root",
        Utc::now(),
    )
    .await
    .unwrap();
    for m in [sam, restricted] {
        db.decide_service_account(m, id, DecisionChange::Create, "root", Utc::now())
            .await
            .unwrap();
    }

    assert!(
        jobs.has_work(JobKind::AccountsReconcile, &db)
            .await
            .unwrap()
    );
    let report = jobs
        .run(JobKind::AccountsReconcile, ctx(&db))
        .await
        .unwrap();
    assert!(
        matches!(report.expectation(), Expectation::Met(_)),
        "{report:?}"
    );
    assert_eq!(report.counter("create"), Some(1));
    assert_eq!(report.counter("undecided"), Some(1));
    assert_eq!(report.counter("conflicts"), Some(1));

    let stored = db.account_plan(id).await.unwrap().unwrap();
    assert!(stored.complete);
    let plan = stored_plan(&stored);
    let entry = |m: i64| plan.entries.iter().find(|e| e.member_id == m).unwrap();
    assert!(matches!(
        entry(alex).state,
        State::Undecided {
            suggestion: Some(_)
        }
    ));
    assert_eq!(entry(alex).steps, vec![]);
    assert_eq!(
        entry(friend).remote_id.as_deref(),
        Some(friend_remote.as_str())
    );
    // friend sees every library in the fixture: restricted to Movies.
    assert!(matches!(
        &entry(friend).steps[0].action,
        Action::SetLibraries { from_all: true, .. }
    ));
    assert!(matches!(
        &entry(sam).steps[0].action,
        Action::Create { username, libraries } if username == "sam"
            && libraries == &["f137a2dd21bbc1b99aa5c0f6bf02a805".to_owned()]
    ));
    assert!(matches!(
        entry(restricted).state,
        State::Conflict {
            conflict: Conflict::NameTaken { .. }
        }
    ));

    // The same input gives the same fingerprint; a decision without steps
    // keeps it (it names what the plan would do), one with steps changes it.
    jobs.run(JobKind::AccountsReconcile, ctx(&db))
        .await
        .unwrap();
    let again = db.account_plan(id).await.unwrap().unwrap();
    assert_eq!(again.fingerprint, stored.fingerprint);
    db.decide_service_account(alex, id, DecisionChange::NoAccount, "root", Utc::now())
        .await
        .unwrap();
    jobs.run(JobKind::AccountsReconcile, ctx(&db))
        .await
        .unwrap();
    assert_eq!(
        db.account_plan(id).await.unwrap().unwrap().fingerprint,
        stored.fingerprint
    );
    let alex_remote = db
        .remote_accounts(id)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.username == "Alex")
        .unwrap()
        .remote_id;
    db.decide_service_account(
        alex,
        id,
        DecisionChange::Link {
            remote_id: &alex_remote,
        },
        "root",
        Utc::now(),
    )
    .await
    .unwrap();
    jobs.run(JobKind::AccountsReconcile, ctx(&db))
        .await
        .unwrap();
    assert_ne!(
        db.account_plan(id).await.unwrap().unwrap().fingerprint,
        stored.fingerprint
    );

    // Planning sent nothing at all to the service.
    assert_eq!(
        fake.server.received_requests().await.unwrap().len(),
        requests_after_scan
    );
    assert_eq!(fake.non_get_requests().await, Vec::<String>::new());
}

#[tokio::test]
async fn without_a_complete_listing_the_plan_creates_nothing() {
    use starr_core::provisioning::{State, WaitReason};
    use starr_db::service_accounts::DecisionChange;

    let db = Database::open_in_memory().await.unwrap();
    let fake = fake_jellyfin().await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = add(&db, &jobs, "jellyfin", &fake.server.uri(), KEY).await;
    watch_in(&db, id).await;
    map_movies(&db, id).await;
    let sam = member(&db, "sam", true).await;
    let mia = member(&db, "mia", false).await;
    db.decide_service_account(sam, id, DecisionChange::Create, "root", Utc::now())
        .await
        .unwrap();
    db.decide_service_account(mia, id, DecisionChange::Create, "root", Utc::now())
        .await
        .unwrap();
    // Never scanned: no complete listing.
    let report = jobs
        .run(JobKind::AccountsReconcile, ctx(&db))
        .await
        .unwrap();
    assert!(
        matches!(report.expectation(), Expectation::Unmet(_)),
        "{report:?}"
    );
    let plan = stored_plan(&db.account_plan(id).await.unwrap().unwrap());
    assert!(!plan.complete);
    assert_eq!(
        plan.entries[0].state,
        State::Waiting {
            reason: WaitReason::ListingIncomplete
        }
    );
    // mia has no movies: nothing needed, whatever the listing.
    assert_eq!(plan.entries[1].state, State::NotNeeded);
    assert!(plan.entries.iter().all(|e| e.steps.is_empty()));
    assert_eq!(fake.server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn a_scan_queues_reconcile_and_plans_go_with_their_role() {
    let db = Database::open_in_memory().await.unwrap();
    let fake = fake_jellyfin().await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    let id = add(&db, &jobs, "jellyfin", &fake.server.uri(), KEY).await;
    watch_in(&db, id).await;
    jobs.run(JobKind::AccountsScan, ctx(&db)).await.unwrap();
    let (queued,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM job WHERE kind = 'accounts.reconcile'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(queued, 1);
    jobs.run(JobKind::AccountsReconcile, ctx(&db))
        .await
        .unwrap();
    assert!(db.account_plan(id).await.unwrap().is_some());
    let role = db.list_roles(None).await.unwrap()[0].id;
    db.remove_role(role).await.unwrap();
    let report = jobs
        .run(JobKind::AccountsReconcile, ctx(&db))
        .await
        .unwrap();
    assert!(matches!(report.expectation(), Expectation::NotApplicable));
    assert_eq!(db.account_plan(id).await.unwrap(), None);
}
