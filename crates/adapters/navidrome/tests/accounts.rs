//! Navidrome `Accounts` contract tests against the stateful local fake
//! (`starr_adapter_kit::contract::navidrome`, never a real Navidrome),
//! seeded from the **derived** fixtures in `fixtures/0.64.2/derived/`
//! (hand-written from Navidrome's source, not recorded). Every trap from
//! `capabilities.md` ("Navidrome") is a case here: no disable flag, the
//! partial-PUT trap, concurrent-create twins, admins never touched, library
//! lists never empty or unknown, passwords never empty.
#![allow(clippy::unwrap_used)]

use secrecy::SecretString;
use serde_json::Value;
use starr_adapter_kit::contract::navidrome::{Derived, NavidromeFake};
use starr_adapter_navidrome::{ACCOUNT_RULES, AUTH, DESCRIPTOR, Navidrome};
use starr_core::accounts::{
    Accounts, CreateOutcome, DisableMode, LibraryGrant, NewAccount, NonEmpty,
};
use starr_core::adapters::AdapterError;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/0.64.2/derived");
const ADMIN_PASSWORD: &str = "admin-pass-0123";
/// The test password set's spirit (`capabilities.md` cross-service 5).
const PASSWORD: &str = " pässwört \"q\" \\ %20 🎵 ";
const ALEX: &str = "3Tz5Wq8Ey1Ru4Io7Pa2Sd6";
const SAM: &str = "9Kd2Lf5Gh8Jz1Xc4Vb7Nm3";
const OPERATOR: &str = "0Fq7Hc2Vd8Jk5Lp3Mn9Rb1";
const STARR: &str = "6aB3kQ9zXw1Lm4Np7Rs2Tu";

async fn fake() -> (NavidromeFake, Navidrome) {
    let fake = NavidromeFake::start(
        Derived::from_dir(FIXTURES),
        &[
            ("starr", ADMIN_PASSWORD),
            ("Alex", "alex-old-password"),
            ("sam", "sam-old-password"),
        ],
    )
    .await;
    let adapter = Navidrome::new(fake.client(&DESCRIPTOR, AUTH, "starr", ADMIN_PASSWORD));
    (fake, adapter)
}

fn libs(ids: &[&str]) -> NonEmpty<String> {
    NonEmpty::new(ids.iter().map(|s| (*s).to_owned()).collect()).unwrap()
}

fn password_of(write: &starr_adapter_kit::contract::media_server::Write) -> String {
    write.body["password"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn is_random(password: &str) -> bool {
    password.len() == 64 && password.chars().all(|c| c.is_ascii_hexdigit())
}

#[test]
fn rules_say_no_flag_per_library_and_sign_out() {
    assert_eq!(ACCOUNT_RULES.disable, DisableMode::ScramblePassword);
    const { assert!(ACCOUNT_RULES.delete && ACCOUNT_RULES.password_change_signs_out) };
    assert!(
        DESCRIPTOR
            .capabilities
            .contains(&starr_core::adapters::Capability::Accounts)
    );
}

#[tokio::test]
async fn the_listing_is_complete_reads_admins_as_privileged_and_never_writes() {
    let (fake, navidrome) = fake().await;
    let listing = navidrome.accounts().await.unwrap();
    assert!(listing.complete);
    let got: Vec<_> = listing
        .items
        .iter()
        .map(|a| {
            (
                a.remote_id.as_str(),
                a.username.as_str(),
                a.enabled,
                a.privileged,
                a.libraries.clone(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            (STARR, "starr", true, true, LibraryGrant::All),
            (OPERATOR, "morgan", true, true, LibraryGrant::All),
            (
                ALEX,
                "Alex",
                true,
                false,
                LibraryGrant::Only(vec!["1".into()])
            ),
            (
                SAM,
                "sam",
                true,
                false,
                LibraryGrant::Only(vec!["1".into(), "2".into()])
            ),
        ]
    );
    assert_eq!(navidrome.account(SAM).await.unwrap(), listing.items[3]);
    assert!(matches!(
        navidrome.account("nope").await,
        Err(AdapterError::NotFound(_))
    ));
    assert_eq!(fake.writes().await, vec![], "reads only");
    assert_eq!(fake.logins(), 1, "one login, the token is reused");
}

#[tokio::test]
async fn users_without_libraries_are_read_per_user_and_unreadable_ones_make_it_incomplete() {
    let (fake, navidrome) = fake().await;
    fake.users_without_libraries();
    // A partial PUT emptied this one's name (the trap): not readable.
    fake.edit_user("sam", |u| {
        u.insert("userName".into(), Value::String(String::new()));
    });
    let listing = navidrome.accounts().await.unwrap();
    assert!(!listing.complete, "an unreadable account is left out");
    let ids: Vec<_> = listing.items.iter().map(|a| a.remote_id.as_str()).collect();
    assert_eq!(ids, vec![STARR, OPERATOR, ALEX]);
    assert_eq!(
        listing.items[2].libraries,
        LibraryGrant::Only(vec!["1".into()])
    );
    let missing_admin = {
        fake.edit_user("Alex", |u| {
            u.remove("isAdmin");
        });
        navidrome.account(ALEX).await.unwrap()
    };
    assert!(missing_admin.privileged, "unknown rights count as admin");
    assert_eq!(fake.writes().await, vec![]);
}

#[tokio::test]
async fn disable_scrambles_with_the_full_object_and_enable_is_refused() {
    let (fake, navidrome) = fake().await;
    let before = fake.user("sam").unwrap();
    let account = navidrome.set_enabled(SAM, false).await.unwrap();
    // Still "enabled": the service has no flag.
    assert!(account.enabled);
    let writes = fake.writes().await;
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(
        (writes[0].method.as_str(), writes[0].target.as_str()),
        ("PUT", format!("/api/user/{SAM}").as_str())
    );
    // The full object as read (the partial-PUT trap), plus the password.
    for field in ["id", "userName", "name", "email", "isAdmin", "createdAt"] {
        assert_eq!(writes[0].body[field], before[field], "{field}");
    }
    let scrambled = password_of(&writes[0]);
    assert!(is_random(&scrambled), "{scrambled:?}");
    assert!(!fake.login_works("sam", "sam-old-password"), "locked out");
    assert_eq!(fake.user("sam").unwrap()["userName"], "sam", "name kept");
    assert_eq!(fake.sign_outs(SAM), 1);
    // Alex is untouched.
    assert!(fake.login_works("Alex", "alex-old-password"));

    // A second disable scrambles again (the planner never asks for it).
    navidrome.set_enabled(SAM, false).await.unwrap();
    let again = fake.writes().await;
    assert_ne!(password_of(&again[1]), scrambled);

    // Enabling is refused without a request.
    let count = fake.writes().await.len();
    assert!(matches!(
        navidrome.set_enabled(ALEX, true).await,
        Err(AdapterError::Rejected(_))
    ));
    assert_eq!(fake.writes().await.len(), count);
}

#[tokio::test]
async fn administrators_are_never_written() {
    let (fake, navidrome) = fake().await;
    for admin in [STARR, OPERATOR] {
        assert!(matches!(
            navidrome.set_enabled(admin, false).await,
            Err(AdapterError::Rejected(_))
        ));
        assert!(matches!(
            navidrome.set_libraries(admin, &libs(&["1"])).await,
            Err(AdapterError::Rejected(_))
        ));
        assert!(matches!(
            navidrome
                .set_password(admin, &SecretString::from(PASSWORD.to_owned()))
                .await,
            Err(AdapterError::Rejected(_))
        ));
    }
    assert_eq!(fake.writes().await, vec![]);
    assert!(fake.login_works("starr", ADMIN_PASSWORD));
}

#[tokio::test]
async fn libraries_are_exact_listed_and_written_only_when_they_differ() {
    let (fake, navidrome) = fake().await;
    // Already granted: nothing written.
    let same = navidrome
        .set_libraries(SAM, &libs(&["2", "1"]))
        .await
        .unwrap();
    assert_eq!(
        same.libraries,
        LibraryGrant::Only(vec!["1".into(), "2".into()])
    );
    assert_eq!(fake.writes().await, vec![]);
    // Unknown or malformed IDs: refused before writing.
    for bad in ["3", "music"] {
        assert!(matches!(
            navidrome.set_libraries(SAM, &libs(&["1", bad])).await,
            Err(AdapterError::Rejected(_))
        ));
    }
    assert_eq!(fake.writes().await, vec![]);

    let account = navidrome
        .set_libraries(SAM, &libs(&["1", "1"]))
        .await
        .unwrap();
    assert_eq!(account.libraries, LibraryGrant::Only(vec!["1".into()]));
    let writes = fake.writes().await;
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].target, format!("/api/user/{SAM}/library"));
    assert_eq!(writes[0].body, serde_json::json!({ "libraryIds": [1] }));
    assert_eq!(fake.grants(SAM), vec![1]);
    assert_eq!(fake.grants(ALEX), vec![1], "alex untouched");
}

#[tokio::test]
async fn passwords_are_written_with_the_full_object_never_empty() {
    let (fake, navidrome) = fake().await;
    assert!(matches!(
        navidrome
            .set_password(ALEX, &SecretString::from(String::new()))
            .await,
        Err(AdapterError::Rejected(_))
    ));
    assert_eq!(fake.writes().await, vec![]);

    navidrome
        .set_password(ALEX, &SecretString::from(PASSWORD.to_owned()))
        .await
        .unwrap();
    assert!(fake.login_works("alex", PASSWORD), "edges and Unicode kept");
    assert!(!fake.login_works("alex", "alex-old-password"));
    assert_eq!(fake.user("Alex").unwrap()["userName"], "Alex");
    let writes = fake.writes().await;
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].body["userName"], "Alex");
    assert!(fake.login_works("sam", "sam-old-password"), "sam untouched");
}

#[tokio::test]
async fn a_write_that_empties_the_name_fails_the_read_back() {
    let (fake, navidrome) = fake().await;
    fake.break_user_writes();
    let err = navidrome
        .set_password(ALEX, &SecretString::from(PASSWORD.to_owned()))
        .await
        .unwrap_err();
    assert!(matches!(err, AdapterError::Unknown(_)), "{err:?}");
    assert!(!err.to_string().contains(PASSWORD.trim()));
    assert!(matches!(
        navidrome.set_enabled(SAM, false).await,
        Err(AdapterError::Unknown(_))
    ));
}

fn new_account<'a>(
    username: &'a str,
    password: &'a SecretString,
    libraries: &'a NonEmpty<String>,
) -> NewAccount<'a> {
    NewAccount {
        username,
        password,
        libraries,
    }
}

#[tokio::test]
async fn create_scrambles_first_then_libraries_then_the_member_password() {
    let (fake, navidrome) = fake().await;
    let password = SecretString::from(PASSWORD.to_owned());
    let only = libs(&["2"]);
    let outcome = navidrome
        .create_account(&new_account("mia", &password, &only))
        .await
        .unwrap();
    let CreateOutcome::Created(account) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(account.username, "mia");
    assert!(!account.privileged);
    assert_eq!(account.libraries, LibraryGrant::Only(vec!["2".into()]));
    let writes = fake.writes().await;
    let steps: Vec<_> = writes
        .iter()
        .map(|w| (w.method.as_str(), w.target.clone()))
        .collect();
    let id = account.remote_id.clone();
    assert_eq!(
        steps,
        vec![
            ("POST", "/api/user".to_owned()),
            ("PUT", format!("/api/user/{id}/library")),
            ("PUT", format!("/api/user/{id}")),
        ]
    );
    // Created with a random password, never as admin; the member's
    // password only after the libraries.
    assert!(is_random(&password_of(&writes[0])));
    assert_eq!(writes[0].body["isAdmin"], false);
    assert_eq!(password_of(&writes[2]), PASSWORD);
    assert_eq!(writes[2].body["userName"], "mia");
    assert!(fake.login_works("mia", PASSWORD));
    assert_eq!(fake.grants(&id), vec![2]);

    // The name is taken now (any case): an outcome, no second account.
    let outcome = navidrome
        .create_account(&new_account("MIA", &password, &only))
        .await
        .unwrap();
    assert_eq!(outcome, CreateOutcome::NameTaken { remote_id: id });
    let named: Vec<_> = fake
        .users()
        .into_iter()
        .filter(|u| u["userName"].as_str().unwrap().eq_ignore_ascii_case("mia"))
        .collect();
    assert_eq!(named.len(), 1);
}

#[tokio::test]
async fn create_refuses_empty_passwords_and_unknown_libraries_before_writing() {
    let (fake, navidrome) = fake().await;
    let empty = SecretString::from(String::new());
    let password = SecretString::from(PASSWORD.to_owned());
    assert!(matches!(
        navidrome
            .create_account(&new_account("mia", &empty, &libs(&["1"])))
            .await,
        Err(AdapterError::Rejected(_))
    ));
    assert!(matches!(
        navidrome
            .create_account(&new_account("mia", &password, &libs(&["7"])))
            .await,
        Err(AdapterError::Rejected(_))
    ));
    assert_eq!(fake.writes().await, vec![]);
}

#[tokio::test]
async fn a_concurrent_twin_wins_and_starr_s_account_keeps_the_random_password() {
    let (fake, navidrome) = fake().await;
    fake.twin_on_create();
    let password = SecretString::from(PASSWORD.to_owned());
    let outcome = navidrome
        .create_account(&new_account("mia", &password, &libs(&["1"])))
        .await
        .unwrap();
    let twin = fake.user("MIA").unwrap()["id"].as_str().unwrap().to_owned();
    assert_eq!(outcome, CreateOutcome::NameTaken { remote_id: twin });
    // Only the scrambled create: the member's password never went out.
    let writes = fake.writes().await;
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert!(!fake.login_works("mia", PASSWORD));
}

#[tokio::test]
async fn a_failed_library_step_leaves_an_unfinished_scrambled_account() {
    let (fake, navidrome) = fake().await;
    fake.fail_library_writes();
    let password = SecretString::from(PASSWORD.to_owned());
    let outcome = navidrome
        .create_account(&new_account("mia", &password, &libs(&["1"])))
        .await
        .unwrap();
    let CreateOutcome::Unfinished { remote_id, error } = outcome else {
        panic!("{outcome:?}");
    };
    assert!(matches!(error, AdapterError::Transient(_)), "{error:?}");
    assert_eq!(fake.user("mia").unwrap()["id"], remote_id.as_str());
    assert!(
        !fake.login_works("mia", PASSWORD),
        "never the member's password"
    );
    assert!(
        fake.writes()
            .await
            .iter()
            .all(|w| password_of(w) != PASSWORD)
    );
}

#[tokio::test]
async fn delete_needs_the_plan_refuses_admins_and_reads_back() {
    use starr_core::members::MemberStatus;
    use starr_core::provisioning::{
        Decision, Instance, Link, Observed, Origin, PasswordNeed, Wanted, delete_plan, plan,
    };
    let (fake, navidrome) = fake().await;
    let accounts = [Observed {
        remote_id: SAM.to_owned(),
        username: "sam".to_owned(),
        enabled: true,
        privileged: false,
        locked_by_service: false,
        libraries: Some(vec!["1".to_owned()]),
        gone: false,
        ignored: false,
        locked_by_starr: true,
    }];
    let planned = plan(
        &Instance {
            listing_complete: true,
            disable: DisableMode::ScramblePassword,
            accounts: &accounts,
        },
        &[(
            Wanted {
                member_id: 1,
                username: "sam".to_owned(),
                status: MemberStatus::Retired,
                libraries: vec![],
                unmapped: vec![],
                deletion_due: true,
            },
            Some(Link {
                decision: Decision::Create,
                remote_id: Some(SAM.to_owned()),
                password: PasswordNeed::Nothing,
                expected: None,
                origin: Some(Origin::Provisioned),
            }),
        )],
    );
    let entry = &planned.entries[0];
    let plan = delete_plan(entry, &entry.steps[0]).unwrap();
    // Another account than the plan names: refused without a request.
    assert!(matches!(
        navidrome.delete_account(ALEX, &plan).await,
        Err(AdapterError::Rejected(_))
    ));
    assert_eq!(fake.writes().await, vec![]);
    navidrome.delete_account(SAM, &plan).await.unwrap();
    assert!(fake.user("sam").is_none());
    let writes = fake.writes().await;
    assert_eq!(writes.len(), 1);
    assert_eq!(
        (writes[0].method.as_str(), writes[0].target.as_str()),
        ("DELETE", format!("/api/user/{SAM}").as_str())
    );
    // Gone already: done without a write.
    navidrome.delete_account(SAM, &plan).await.unwrap();
    assert_eq!(fake.writes().await.len(), 1);
}
