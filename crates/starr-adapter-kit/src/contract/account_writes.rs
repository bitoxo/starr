//! Generic contract suite for the write half of [`Accounts`] (spec 0005
//! slice 4, D2, D10–D12, D17) against a [`MediaServerFake`] seeded from the
//! adapter's recorded fixtures. Every case asserts the **exact** requests
//! other than `GET` the adapter sent: refused writes send none, writes
//! already in effect send none, and a policy write sends the full policy as
//! read with only Starr's fields changed.

use secrecy::SecretString;
use serde_json::{Value, json};
use starr_core::accounts::{
    Accounts, CreateOutcome, DeletePlan, DisableMode, LibraryGrant, NewAccount, NonEmpty,
};
use starr_core::adapters::{AdapterDescriptor, AdapterError};
use starr_core::members::MemberStatus;
use starr_core::provisioning::{
    Decision, Instance, Link, Observed, Origin, PasswordNeed, Wanted, delete_plan, plan,
};

use super::media_server::{FakeFlavor, MediaServerFake, Recorded, Write};
use crate::{AuthScheme, ServiceClient};

const KEY: &str = "contract-test-key-0123456789abcdef";
/// The test password set's spirit (`capabilities.md` cross-service 5):
/// Unicode, quotes, backslash, an encoded space, edges kept.
const PASSWORD: &str = " pässwört \"q\" \\ %20 🎬 ";

/// Everything the suite needs to know about one adapter.
#[derive(Debug)]
pub struct WritesContract<A> {
    pub descriptor: &'static AdapterDescriptor,
    pub auth: AuthScheme,
    pub build: fn(ServiceClient) -> A,
    pub flavor: FakeFlavor,
    /// Directory with the recorded `users.json`, `system_info.json`,
    /// `virtual_folders.json`.
    pub recorded_dir: String,
    /// A plain, enabled, unprivileged account seeing every library; also
    /// the template for new accounts.
    pub plain: &'static str,
    /// An administrator.
    pub admin: &'static str,
    /// An account locked by the service, where the service shows locks.
    pub locked: Option<&'static str>,
    /// Two grant IDs the service lists.
    pub libraries: [&'static str; 2],
}

impl<A: Accounts> WritesContract<A> {
    async fn fake(&self) -> (MediaServerFake, A) {
        let fake = MediaServerFake::start(
            self.flavor,
            self.auth,
            KEY,
            Recorded::from_dir(&self.recorded_dir),
            self.plain,
        )
        .await;
        let adapter = (self.build)(fake.client(self.descriptor, KEY));
        (fake, adapter)
    }

    fn password_target(&self, id: &str) -> String {
        match self.flavor {
            FakeFlavor::Jellyfin => format!("/Users/Password?userId={id}"),
            FakeFlavor::Emby => format!("/Users/{id}/Password"),
        }
    }
}

fn id_of(fake: &MediaServerFake, name: &str) -> String {
    let user = fake
        .user(name)
        .unwrap_or_else(|| panic!("no recorded account {name:?}"));
    user["Id"].as_str().unwrap_or_default().to_owned()
}

fn policy_of(fake: &MediaServerFake, name: &str) -> Value {
    fake.user(name)
        .map_or_else(|| panic!("no account {name:?}"), |u| u["Policy"].clone())
}

fn libraries(ids: &[&str]) -> NonEmpty<String> {
    NonEmpty::new(ids.iter().map(|s| (*s).to_owned()).collect())
        .unwrap_or_else(|| panic!("empty library list in the suite"))
}

/// The one policy write a case expects: the policy before, with `changes`.
fn policy_write(id: &str, before: &Value, changes: &Value) -> Write {
    let mut body = before.clone();
    if let (Some(body), Some(changes)) = (body.as_object_mut(), changes.as_object()) {
        for (k, v) in changes {
            body.insert(k.clone(), v.clone());
        }
    }
    Write {
        method: "POST".to_owned(),
        target: format!("/Users/{id}/Policy"),
        body,
    }
}

/// Run every write case. Panics with a description on failure.
pub async fn run_writes_contract<A: Accounts>(c: &WritesContract<A>) {
    disable_and_enable_write_the_full_policy(c).await;
    libraries_are_an_explicit_list_and_unknown_ids_are_refused(c).await;
    fields_set_in_the_service_survive_a_write(c).await;
    administrators_are_never_written(c).await;
    a_lockout_is_never_lifted(c).await;
    passwords_go_to_the_right_route_and_are_never_empty(c).await;
    create_follows_the_safe_order(c).await;
    creating_a_taken_name_creates_nothing(c).await;
    a_write_the_service_ignored_fails_the_read_back(c).await;
    a_create_whose_policy_write_fails_leaves_no_usable_account(c).await;
    a_create_failing_after_the_disable_still_gets_a_random_password(c).await;
    deletes_need_the_plan_of_a_due_deletion_and_read_back(c).await;
}

/// The only way to a `DeletePlan`: the planner, for a member whose deletion
/// order is due and who owns `remote_id` (as Starr believes it, i.e. from
/// the last scan).
fn due_delete(remote_id: &str) -> DeletePlan {
    let accounts = [Observed {
        remote_id: remote_id.to_owned(),
        username: "leaving".to_owned(),
        enabled: false,
        privileged: false,
        locked_by_service: false,
        libraries: None,
        gone: false,
        ignored: false,
        locked_by_starr: false,
    }];
    let wanted = Wanted {
        member_id: 1,
        username: "leaving".to_owned(),
        status: MemberStatus::Retired,
        libraries: vec![],
        unmapped: vec![],
        deletion_due: true,
    };
    let link = Link {
        decision: Decision::Linked,
        remote_id: Some(remote_id.to_owned()),
        password: PasswordNeed::Nothing,
        expected: None,
        origin: Some(Origin::Provisioned),
    };
    let planned = plan(
        &Instance {
            listing_complete: true,
            disable: DisableMode::Flag,
            accounts: &accounts,
        },
        &[(wanted, Some(link))],
    );
    let entry = &planned.entries[0];
    entry
        .steps
        .first()
        .and_then(|step| delete_plan(entry, step))
        .unwrap_or_else(|| panic!("the planner planned no delete: {entry:?}"))
}

/// D16: `DELETE /Users/{id}` only for the account the plan names, never
/// for an administrator (the account may have gained admin rights since
/// the scan), read back as gone; a second delete writes nothing.
async fn deletes_need_the_plan_of_a_due_deletion_and_read_back<A: Accounts>(c: &WritesContract<A>) {
    let (fake, adapter) = c.fake().await;
    assert!(
        adapter.account_rules().delete,
        "the adapter deletes accounts"
    );
    let id = id_of(&fake, c.plain);
    let admin = id_of(&fake, c.admin);
    // A plan for another account refuses before any request.
    let before = fake.request_count().await;
    let r = adapter.delete_account(&id, &due_delete(&admin)).await;
    assert!(matches!(r, Err(AdapterError::Rejected(_))), "other: {r:?}");
    assert_eq!(fake.request_count().await, before);
    // An administrator is refused after reading it.
    let r = adapter.delete_account(&admin, &due_delete(&admin)).await;
    assert!(matches!(r, Err(AdapterError::Rejected(_))), "admin: {r:?}");
    assert_eq!(fake.writes().await, vec![], "no write to an administrator");
    adapter
        .delete_account(&id, &due_delete(&id))
        .await
        .unwrap_or_else(|e| panic!("delete: {e:?}"));
    assert_eq!(
        fake.writes().await,
        vec![Write {
            method: "DELETE".to_owned(),
            target: format!("/Users/{id}"),
            body: Value::Null,
        }]
    );
    assert!(fake.user(c.plain).is_none(), "the account is gone");
    // Gone already: nothing written again.
    adapter
        .delete_account(&id, &due_delete(&id))
        .await
        .unwrap_or_else(|e| panic!("delete again: {e:?}"));
    assert_eq!(
        fake.writes().await.len(),
        1,
        "a second delete writes nothing"
    );
    // A delete the service answers but does not apply fails the read-back.
    let (fake, adapter) = c.fake().await;
    fake.ignore_deletes();
    let id = id_of(&fake, c.plain);
    let r = adapter.delete_account(&id, &due_delete(&id)).await;
    assert!(matches!(r, Err(AdapterError::Unknown(_))), "ignored: {r:?}");
}

/// A create that fails after the account was already disabled (here: the
/// final enable) still leaves it with a random password nobody knows, not
/// only disabled (slice 5 open point: "disable ok, random password
/// anyway").
async fn a_create_failing_after_the_disable_still_gets_a_random_password<A: Accounts>(
    c: &WritesContract<A>,
) {
    let (fake, adapter) = c.fake().await;
    // The first policy write (disable + libraries) succeeds, the enable fails.
    fake.fail_policy_writes_after(1);
    let password = SecretString::from(PASSWORD.to_owned());
    let libs = libraries(&c.libraries[..1]);
    let outcome = adapter
        .create_account(&NewAccount {
            username: "newfriend",
            password: &password,
            libraries: &libs,
        })
        .await
        .unwrap_or_else(|e| panic!("create: {e:?}"));
    let CreateOutcome::Unfinished { remote_id, .. } = outcome else {
        panic!("unfinished create: {outcome:?}");
    };
    let Some(user) = fake.user("newfriend") else {
        panic!("the account exists");
    };
    assert_eq!(user["Policy"]["IsDisabled"], json!(true), "stays disabled");
    let writes = fake.writes().await;
    let Some(last) = writes.last() else {
        panic!("no writes");
    };
    assert_eq!(last.target, c.password_target(&remote_id));
    let scrambled = last.body["NewPw"].as_str().unwrap_or_default();
    assert_eq!(scrambled.len(), 64, "a random password: {scrambled:?}");
    assert_ne!(scrambled, PASSWORD);
    // The disable was already in effect: no second disable was written.
    let disables = writes
        .iter()
        .filter(|w| w.body.get("IsDisabled") == Some(&json!(true)))
        .count();
    assert_eq!(disables, 1, "{writes:?}");
}

/// A create whose first policy write fails leaves no account anyone can
/// use: the disable is tried again, and when that fails too the account
/// gets a random password nobody knows (Emby created it active with an
/// empty password, Jellyfin active with every library). The account is
/// reported as unfinished, so Starr owns it and keeps it disabled.
async fn a_create_whose_policy_write_fails_leaves_no_usable_account<A: Accounts>(
    c: &WritesContract<A>,
) {
    let (fake, adapter) = c.fake().await;
    fake.fail_policy_writes();
    let password = SecretString::from(PASSWORD.to_owned());
    let libs = libraries(&c.libraries[..1]);
    let outcome = adapter
        .create_account(&NewAccount {
            username: "newfriend",
            password: &password,
            libraries: &libs,
        })
        .await
        .unwrap_or_else(|e| panic!("create: {e:?}"));
    let CreateOutcome::Unfinished { remote_id, error } = outcome else {
        panic!("unfinished create: {outcome:?}");
    };
    assert!(matches!(error, AdapterError::Transient(_)), "{error:?}");
    let Some(user) = fake.user("newfriend") else {
        panic!("the account exists");
    };
    assert_eq!(user["Id"], json!(remote_id));
    let writes = fake.writes().await;
    let Some(last) = writes.last() else {
        panic!("no writes");
    };
    assert_eq!(last.target, c.password_target(&remote_id));
    let scrambled = last.body["NewPw"].as_str().unwrap_or_default();
    assert_eq!(scrambled.len(), 64, "a random password: {scrambled:?}");
    assert_ne!(scrambled, PASSWORD);
    assert_eq!(user["HasPassword"], json!(true));
    // Never the member's password outside the create body (Jellyfin).
    for w in &writes[1..] {
        assert!(!w.body.to_string().contains("pässwört"), "{w:?}");
    }
}

async fn disable_and_enable_write_the_full_policy<A: Accounts>(c: &WritesContract<A>) {
    let (fake, adapter) = c.fake().await;
    let id = id_of(&fake, c.plain);
    let before = policy_of(&fake, c.plain);
    let account = adapter
        .set_enabled(&id, false)
        .await
        .unwrap_or_else(|e| panic!("disable: {e:?}"));
    assert!(!account.enabled, "read back disabled");
    let mut changes = json!({ "IsDisabled": true });
    if c.flavor == FakeFlavor::Jellyfin {
        // Reset so a disabled account never looks locked later.
        changes["InvalidLoginAttemptCount"] = json!(0);
    }
    assert_eq!(
        fake.writes().await,
        vec![policy_write(&id, &before, &changes)]
    );
    // Already disabled: nothing is written.
    adapter
        .set_enabled(&id, false)
        .await
        .unwrap_or_else(|e| panic!("disable again: {e:?}"));
    assert_eq!(
        fake.writes().await.len(),
        1,
        "a repeated disable writes nothing"
    );
    let disabled = policy_of(&fake, c.plain);
    let account = adapter
        .set_enabled(&id, true)
        .await
        .unwrap_or_else(|e| panic!("enable: {e:?}"));
    assert!(account.enabled);
    let writes = fake.writes().await;
    assert_eq!(writes.len(), 2);
    assert_eq!(
        writes[1],
        policy_write(&id, &disabled, &json!({ "IsDisabled": false }))
    );
}

async fn libraries_are_an_explicit_list_and_unknown_ids_are_refused<A: Accounts>(
    c: &WritesContract<A>,
) {
    let (fake, adapter) = c.fake().await;
    let id = id_of(&fake, c.plain);
    let before = policy_of(&fake, c.plain);
    let [first, second] = c.libraries;
    let account = adapter
        .set_libraries(&id, &libraries(&[first]))
        .await
        .unwrap_or_else(|e| panic!("set libraries: {e:?}"));
    assert_eq!(
        account.libraries,
        LibraryGrant::Only(vec![first.to_owned()])
    );
    assert_eq!(
        fake.writes().await,
        vec![policy_write(
            &id,
            &before,
            &json!({ "EnableAllFolders": false, "EnabledFolders": [first] })
        )]
    );
    // The same list again (any order, duplicates dropped): nothing written.
    adapter
        .set_libraries(&id, &libraries(&[first, first]))
        .await
        .unwrap_or_else(|e| panic!("same libraries: {e:?}"));
    assert_eq!(fake.writes().await.len(), 1);
    // An ID the service does not list is refused before any write.
    let result = adapter
        .set_libraries(
            &id,
            &libraries(&[second, "0123456789abcdef0123456789abcdef"]),
        )
        .await;
    assert!(
        matches!(result, Err(AdapterError::Rejected(_))),
        "unknown library: {result:?}"
    );
    assert_eq!(
        fake.writes().await.len(),
        1,
        "an unknown library writes nothing"
    );
}

async fn fields_set_in_the_service_survive_a_write<A: Accounts>(c: &WritesContract<A>) {
    // The partial-policy trap (Emby resets every field not sent): fields
    // an admin set in the service stay as they are.
    let (fake, adapter) = c.fake().await;
    fake.edit_user(c.plain, |u| {
        u["Policy"]["EnableRemoteAccess"] = json!(false);
        u["Policy"]["EnableContentDownloading"] = json!(false);
        u["Policy"]["BlockedTags"] = json!(["nsfw"]);
    });
    let id = id_of(&fake, c.plain);
    adapter
        .set_libraries(&id, &libraries(&c.libraries))
        .await
        .unwrap_or_else(|e| panic!("set libraries: {e:?}"));
    adapter
        .set_enabled(&id, false)
        .await
        .unwrap_or_else(|e| panic!("disable: {e:?}"));
    let after = policy_of(&fake, c.plain);
    assert_eq!(after["EnableRemoteAccess"], json!(false));
    assert_eq!(after["EnableContentDownloading"], json!(false));
    assert_eq!(after["BlockedTags"], json!(["nsfw"]));
    assert_eq!(after["EnableAllFolders"], json!(false));
}

async fn administrators_are_never_written<A: Accounts>(c: &WritesContract<A>) {
    let (fake, adapter) = c.fake().await;
    let id = id_of(&fake, c.admin);
    let refused = |r: &Result<(), AdapterError>| matches!(r, Err(AdapterError::Rejected(_)));
    let r = adapter.set_enabled(&id, false).await.map(|_| ());
    assert!(refused(&r), "disable admin: {r:?}");
    let r = adapter
        .set_libraries(&id, &libraries(&c.libraries[..1]))
        .await
        .map(|_| ());
    assert!(refused(&r), "libraries admin: {r:?}");
    let r = adapter
        .set_password(&id, &SecretString::from(PASSWORD.to_owned()))
        .await;
    assert!(refused(&r), "password admin: {r:?}");
    assert_eq!(fake.writes().await, vec![], "no write to an administrator");
}

async fn a_lockout_is_never_lifted<A: Accounts>(c: &WritesContract<A>) {
    let Some(locked) = c.locked else { return };
    let (fake, adapter) = c.fake().await;
    let id = id_of(&fake, locked);
    let r = adapter.set_enabled(&id, true).await;
    assert!(
        matches!(r, Err(AdapterError::Rejected(_))),
        "enable locked: {r:?}"
    );
    assert_eq!(fake.writes().await, vec![], "a lockout is never lifted");
}

async fn passwords_go_to_the_right_route_and_are_never_empty<A: Accounts>(c: &WritesContract<A>) {
    let (fake, adapter) = c.fake().await;
    let id = id_of(&fake, c.plain);
    let before = fake.request_count().await;
    let r = adapter
        .set_password(&id, &SecretString::from(String::new()))
        .await;
    assert!(matches!(r, Err(AdapterError::Rejected(_))), "empty: {r:?}");
    assert_eq!(
        fake.request_count().await,
        before,
        "an empty password sends no request at all"
    );
    adapter
        .set_password(&id, &SecretString::from(PASSWORD.to_owned()))
        .await
        .unwrap_or_else(|e| panic!("password: {e:?}"));
    assert_eq!(
        fake.writes().await,
        vec![Write {
            method: "POST".to_owned(),
            target: c.password_target(&id),
            body: json!({ "NewPw": PASSWORD }),
        }]
    );
}

async fn create_follows_the_safe_order<A: Accounts>(c: &WritesContract<A>) {
    let (fake, adapter) = c.fake().await;
    let password = SecretString::from(PASSWORD.to_owned());
    let libs = libraries(&c.libraries[..1]);
    let outcome = adapter
        .create_account(&NewAccount {
            username: "newfriend",
            password: &password,
            libraries: &libs,
        })
        .await
        .unwrap_or_else(|e| panic!("create: {e:?}"));
    let CreateOutcome::Created(account) = outcome else {
        panic!("create: {outcome:?}");
    };
    assert!(account.enabled && !account.privileged);
    assert_eq!(
        account.libraries,
        LibraryGrant::Only(vec![c.libraries[0].to_owned()])
    );
    let id = account.remote_id;
    let writes = fake.writes().await;
    let targets: Vec<(&str, &str)> = writes
        .iter()
        .map(|w| (w.method.as_str(), w.target.as_str()))
        .collect();
    let policy = format!("/Users/{id}/Policy");
    let password_target = c.password_target(&id);
    let expected: Vec<(&str, &str)> = match c.flavor {
        // The password travels with the create.
        FakeFlavor::Jellyfin => vec![("POST", "/Users/New"), ("POST", &policy), ("POST", &policy)],
        // Created active without a password: disable before the password.
        FakeFlavor::Emby => vec![
            ("POST", "/Users/New"),
            ("POST", &policy),
            ("POST", &password_target),
            ("POST", &policy),
        ],
    };
    assert_eq!(targets, expected);
    let create_body = match c.flavor {
        FakeFlavor::Jellyfin => json!({ "Name": "newfriend", "Password": PASSWORD }),
        FakeFlavor::Emby => json!({ "Name": "newfriend" }),
    };
    assert_eq!(writes[0].body, create_body);
    // The first policy write disables with exactly the libraries.
    assert_eq!(writes[1].body["IsDisabled"], json!(true));
    assert_eq!(writes[1].body["EnableAllFolders"], json!(false));
    assert_eq!(writes[1].body["EnabledFolders"], json!([c.libraries[0]]));
    // Enabled last, libraries untouched.
    let last = &writes[writes.len() - 1].body;
    assert_eq!(last["IsDisabled"], json!(false));
    assert_eq!(last["EnabledFolders"], json!([c.libraries[0]]));
    if c.flavor == FakeFlavor::Emby {
        assert_eq!(writes[2].body, json!({ "NewPw": PASSWORD }));
    }
    // Empty passwords and unknown libraries are refused before any request.
    let before = fake.request_count().await;
    let empty = SecretString::from(String::new());
    let r = adapter
        .create_account(&NewAccount {
            username: "other",
            password: &empty,
            libraries: &libs,
        })
        .await;
    assert!(matches!(r, Err(AdapterError::Rejected(_))), "empty: {r:?}");
    assert_eq!(fake.request_count().await, before);
}

async fn creating_a_taken_name_creates_nothing<A: Accounts>(c: &WritesContract<A>) {
    let (fake, adapter) = c.fake().await;
    let password = SecretString::from(PASSWORD.to_owned());
    let libs = libraries(&c.libraries[..1]);
    let existing = id_of(&fake, c.plain);
    let count = fake.users().len();
    // Any case.
    let name = c.plain.to_uppercase();
    let outcome = adapter
        .create_account(&NewAccount {
            username: &name,
            password: &password,
            libraries: &libs,
        })
        .await
        .unwrap_or_else(|e| panic!("create taken: {e:?}"));
    assert_eq!(
        outcome,
        CreateOutcome::NameTaken {
            remote_id: existing
        }
    );
    assert_eq!(fake.users().len(), count, "create twice → one account");
    let writes = fake.writes().await;
    assert_eq!(writes.len(), 1, "only the refused create: {writes:?}");

    if c.flavor == FakeFlavor::Emby {
        // A twin created concurrently (D17): the oldest wins; Starr's
        // younger one stays disabled with a password nobody knows.
        let (fake, adapter) = c.fake().await;
        fake.allow_duplicates();
        let mut twin = fake.user(c.plain).unwrap_or_default();
        twin["Name"] = json!("twin");
        twin["Id"] = json!("70000000000000000000000000000001");
        twin["DateCreated"] = json!("2026-01-01T00:00:00.0000000Z");
        fake.add_user(twin);
        let outcome = adapter
            .create_account(&NewAccount {
                username: "twin",
                password: &password,
                libraries: &libs,
            })
            .await
            .unwrap_or_else(|e| panic!("create twin: {e:?}"));
        assert_eq!(
            outcome,
            CreateOutcome::NameTaken {
                remote_id: "70000000000000000000000000000001".to_owned()
            }
        );
        let mine = fake
            .users()
            .into_iter()
            .find(|u| u["Name"] == "twin" && u["Id"] != "70000000000000000000000000000001")
            .unwrap_or_else(|| panic!("Starr's twin was not created"));
        assert_eq!(
            mine["Policy"]["IsDisabled"],
            json!(true),
            "twin stays disabled"
        );
        let writes = fake.writes().await;
        let password_write = writes
            .iter()
            .find(|w| w.target.ends_with("/Password"))
            .unwrap_or_else(|| panic!("the twin got no password: {writes:?}"));
        let written = password_write.body["NewPw"].as_str().unwrap_or_default();
        assert!(
            written != PASSWORD && written.len() >= 32,
            "a random password"
        );
        assert!(
            !writes
                .iter()
                .any(|w| w.body.get("IsDisabled") == Some(&json!(false))),
            "the twin is never enabled"
        );
    }
}

async fn a_write_the_service_ignored_fails_the_read_back<A: Accounts>(c: &WritesContract<A>) {
    let (fake, adapter) = c.fake().await;
    fake.ignore_policy_writes();
    let id = id_of(&fake, c.plain);
    let r = adapter.set_enabled(&id, false).await;
    assert!(
        matches!(r, Err(AdapterError::Unknown(_))),
        "ignored write: {r:?}"
    );
    let (fake, adapter) = c.fake().await;
    fake.fail_policy_writes();
    let r = adapter.set_enabled(&id_of(&fake, c.plain), false).await;
    assert!(
        matches!(r, Err(AdapterError::Transient(_))),
        "failed write: {r:?}"
    );
}
