//! Account writes Jellyfin and Emby share (spec 0005 slice 4, D11, D17):
//! read–modify–write of the full user policy, safe create order, twins,
//! passwords. Every write reads the account first (an administrator is
//! refused before anything is written, D2) and reads back afterwards
//! (lesson 9). Evidence: `docs/concept/capabilities.md`, "Jellyfin" and
//! "Emby" (traps).
//!
//! Neither service has an `ETag` on policies: a change the service makes in
//! the milliseconds between Starr's read and its write (e.g. a Jellyfin
//! lockout after failed logins) is overwritten and cannot be detected
//! afterwards (`capabilities.md`, Jellyfin traps). The read is taken right
//! before each write to keep that window minimal.
//!
//! Requests sent (and nothing else):
//! - `GET /Users/{id}`, `GET /Users`, `GET /Library/VirtualFolders` (reads);
//! - `POST /Users/{id}/Policy` with the full policy as read, only
//!   `IsDisabled`, `EnableAllFolders`, `EnabledFolders` (and Jellyfin's
//!   `InvalidLoginAttemptCount` when disabling) changed;
//! - `POST /Users/New` (`{Name, Password}` Jellyfin, `{Name}` Emby);
//! - Jellyfin `POST /Users/Password?userId={id}`, Emby
//!   `POST /Users/{id}/Password`, body `{NewPw}`;
//! - `DELETE /Users/{id}` (only with a `DeletePlan` for that account).

use std::collections::BTreeSet;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use starr_core::accounts::{
    CreateOutcome, DeletePlan, LibraryGrant, NewAccount, NonEmpty, RemoteAccount,
};
use starr_core::adapters::AdapterError;
use starr_core::library::encode_component;

use super::{GrantKey, Lockout, User, VirtualFolder, account, libraries};
use crate::ServiceClient;

/// Which of the two services, where their account writes differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Jellyfin,
    Emby,
}

impl Flavor {
    const fn lockout(self) -> Lockout {
        match self {
            Self::Jellyfin => Lockout::DisablesAccount,
            Self::Emby => Lockout::NotReadable,
        }
    }

    const fn grant(self) -> GrantKey {
        match self {
            Self::Jellyfin => GrantKey::ItemId,
            Self::Emby => GrantKey::Guid,
        }
    }
}

/// Serializes creates per instance (D17: Emby creates twins concurrently,
/// Jellyfin answers the second of two with a 500).
const CREATE_LOCK: &str = "accounts.create";

/// One user as the service returned it: the full JSON (for read–modify–
/// write) and what Starr reads from it.
struct Fetched {
    raw: Value,
    account: RemoteAccount,
}

impl Fetched {
    fn parse(raw: Value, lockout: Lockout) -> Result<Self, AdapterError> {
        let user = User::deserialize(&raw)
            .map_err(|err| AdapterError::Unknown(format!("user is not readable: {err}")))?;
        let account = account(user, lockout)?;
        Ok(Self { raw, account })
    }

    /// The full policy object, to be written back with Starr's fields
    /// changed (both services need the whole object: Jellyfin refuses a
    /// partial one, Emby resets every missing field).
    fn into_policy(self) -> Result<Map<String, Value>, AdapterError> {
        match self.raw {
            Value::Object(mut user) => match user.remove("Policy") {
                Some(Value::Object(policy)) => Ok(policy),
                _ => Err(AdapterError::Unknown(
                    "user without a policy object".to_owned(),
                )),
            },
            _ => Err(AdapterError::Unknown("user is not an object".to_owned())),
        }
    }

    fn has_password(&self) -> Option<bool> {
        self.raw.get("HasPassword").and_then(Value::as_bool)
    }

    fn created(&self) -> Option<&str> {
        self.raw.get("DateCreated").and_then(Value::as_str)
    }
}

fn user_path(remote_id: &str) -> String {
    format!("Users/{}", encode_component(remote_id))
}

async fn fetch(
    client: &ServiceClient,
    remote_id: &str,
    lockout: Lockout,
) -> Result<Fetched, AdapterError> {
    let raw: Value = client
        .get_json_not_found_on(&user_path(remote_id), reqwest::StatusCode::BAD_REQUEST)
        .await?;
    let fetched = Fetched::parse(raw, lockout)?;
    if fetched.account.remote_id != remote_id {
        return Err(AdapterError::Unknown(
            "the service answered with another account".to_owned(),
        ));
    }
    Ok(fetched)
}

/// Read the account to write; an administrator is refused (D2).
async fn fetch_writable(
    client: &ServiceClient,
    remote_id: &str,
    lockout: Lockout,
) -> Result<Fetched, AdapterError> {
    let fetched = fetch(client, remote_id, lockout).await?;
    if fetched.account.privileged {
        return Err(AdapterError::Rejected(
            "the account is an administrator in the service; Starr never touches it".to_owned(),
        ));
    }
    Ok(fetched)
}

async fn write_policy(
    client: &ServiceClient,
    remote_id: &str,
    policy: Map<String, Value>,
) -> Result<(), AdapterError> {
    let path = format!("{}/Policy", user_path(remote_id));
    client
        .post_json_no_reply(&path, &[], &Value::Object(policy), false)
        .await
}

fn read_back_differs(what: &str) -> AdapterError {
    AdapterError::Unknown(format!(
        "read-back after the write shows {what} unchanged; the service did not apply it"
    ))
}

/// Enable or disable. Enabling a locked account is refused (D10);
/// disabling on Jellyfin resets the failed-login counter, so the account
/// does not look locked later (`capabilities.md`, Jellyfin traps).
pub async fn set_enabled(
    client: &ServiceClient,
    flavor: Flavor,
    remote_id: &str,
    enabled: bool,
) -> Result<RemoteAccount, AdapterError> {
    let current = fetch_writable(client, remote_id, flavor.lockout()).await?;
    if enabled && current.account.locked_by_service {
        return Err(AdapterError::Rejected(
            "the account is locked by the service after failed logins; Starr never lifts that"
                .to_owned(),
        ));
    }
    if current.account.enabled == enabled {
        return Ok(current.account);
    }
    let mut policy = current.into_policy()?;
    policy.insert("IsDisabled".to_owned(), Value::Bool(!enabled));
    if !enabled && flavor == Flavor::Jellyfin {
        policy.insert("InvalidLoginAttemptCount".to_owned(), json!(0));
    }
    write_policy(client, remote_id, policy).await?;
    let after = fetch(client, remote_id, flavor.lockout()).await?.account;
    if after.enabled != enabled {
        return Err(read_back_differs("enabled"));
    }
    Ok(after)
}

/// The grant IDs the service lists; refuses a list with an ID it does not
/// list (both services store unknown IDs silently, D11). Duplicates are
/// dropped.
async fn checked_libraries(
    client: &ServiceClient,
    flavor: Flavor,
    wanted: &NonEmpty<String>,
) -> Result<Vec<String>, AdapterError> {
    let folders: Vec<VirtualFolder> = client.get_json("Library/VirtualFolders").await?;
    let known: BTreeSet<String> = libraries(folders, flavor.grant())
        .items
        .into_iter()
        .filter_map(|l| l.grant_id)
        .collect();
    let mut out = Vec::with_capacity(wanted.as_slice().len());
    for id in wanted.as_slice() {
        if !known.contains(id) {
            return Err(AdapterError::Rejected(format!(
                "library {id:?} is not listed by the service (renamed or removed?)"
            )));
        }
        if !out.contains(id) {
            out.push(id.clone());
        }
    }
    Ok(out)
}

fn grants(account: &RemoteAccount, wanted: &[String]) -> bool {
    match &account.libraries {
        LibraryGrant::All => false,
        LibraryGrant::Only(ids) => {
            ids.iter().collect::<BTreeSet<_>>() == wanted.iter().collect::<BTreeSet<_>>()
        }
    }
}

fn grant_policy(policy: &mut Map<String, Value>, libraries: &[String]) {
    policy.insert("EnableAllFolders".to_owned(), Value::Bool(false));
    policy.insert("EnabledFolders".to_owned(), json!(libraries));
}

/// Grant exactly `wanted` (explicit list, `EnableAllFolders: false`).
pub async fn set_libraries(
    client: &ServiceClient,
    flavor: Flavor,
    remote_id: &str,
    wanted: &NonEmpty<String>,
) -> Result<RemoteAccount, AdapterError> {
    let wanted = checked_libraries(client, flavor, wanted).await?;
    let current = fetch_writable(client, remote_id, flavor.lockout()).await?;
    if grants(&current.account, &wanted) {
        return Ok(current.account);
    }
    let mut policy = current.into_policy()?;
    grant_policy(&mut policy, &wanted);
    write_policy(client, remote_id, policy).await?;
    let after = fetch(client, remote_id, flavor.lockout()).await?.account;
    if !grants(&after, &wanted) {
        return Err(read_back_differs("the library list"));
    }
    Ok(after)
}

fn refuse_empty(password: &SecretString) -> Result<(), AdapterError> {
    if password.expose_secret().is_empty() {
        return Err(AdapterError::Rejected(
            "an empty password is never written".to_owned(),
        ));
    }
    Ok(())
}

async fn write_password(
    client: &ServiceClient,
    flavor: Flavor,
    remote_id: &str,
    password: &SecretString,
) -> Result<(), AdapterError> {
    let body = json!({ "NewPw": password.expose_secret() });
    match flavor {
        // The newer route; the API key needs no current password.
        Flavor::Jellyfin => {
            client
                .post_json_no_reply(
                    "Users/Password",
                    &[("userId", remote_id.to_owned())],
                    &body,
                    true,
                )
                .await
        }
        // Only ever with the API key: a user token would change the
        // caller's own password whatever the path says.
        Flavor::Emby => {
            let path = format!("{}/Password", user_path(remote_id));
            client.post_json_no_reply(&path, &[], &body, true).await
        }
    }
}

/// Check after a password write. `HasPassword` is read where the service
/// reports it; whether it is reliable is not verified yet
/// (`capabilities.md` [?]), so only an explicit `false` fails.
async fn password_read_back(
    client: &ServiceClient,
    flavor: Flavor,
    remote_id: &str,
) -> Result<Fetched, AdapterError> {
    let after = fetch(client, remote_id, flavor.lockout()).await?;
    if after.has_password() == Some(false) {
        return Err(read_back_differs("the password"));
    }
    Ok(after)
}

/// Set the password of an existing account (signs out its devices).
pub async fn set_password(
    client: &ServiceClient,
    flavor: Flavor,
    remote_id: &str,
    password: &SecretString,
) -> Result<(), AdapterError> {
    refuse_empty(password)?;
    fetch_writable(client, remote_id, flavor.lockout()).await?;
    write_password(client, flavor, remote_id, password).await?;
    password_read_back(client, flavor, remote_id).await?;
    Ok(())
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// The accounts named `name` (any case), as listed.
async fn named(
    client: &ServiceClient,
    flavor: Flavor,
    name: &str,
) -> Result<Vec<Fetched>, AdapterError> {
    let users: Vec<Value> = match client.get_json::<Value>("Users").await? {
        Value::Array(users) => users,
        Value::Object(mut query) => match query.remove("Items") {
            Some(Value::Array(users)) => users,
            _ => return Err(AdapterError::Unknown("unreadable user listing".to_owned())),
        },
        _ => return Err(AdapterError::Unknown("unreadable user listing".to_owned())),
    };
    Ok(users
        .into_iter()
        .filter_map(|raw| Fetched::parse(raw, flavor.lockout()).ok())
        .filter(|f| same_name(&f.account.username, name))
        .collect())
}

/// After a failed step behind a successful create: leave the new account
/// unusable. Disabled, and always a random password nobody knows (other
/// route), so the account stays unusable even if something enables it
/// later (an older Starr after a rollback, an admin in the service): no
/// active account with an empty password (Emby) or with every library and
/// a known password (Jellyfin) stays behind. Best effort; the original
/// error is what counts, and Starr keeps the account disabled afterwards.
async fn leave_unusable(client: &ServiceClient, flavor: Flavor, remote_id: &str) {
    if let Err(err) = set_enabled(client, flavor, remote_id, false).await {
        tracing::error!(
            code = err.code(),
            "a new account could not be disabled after a failed create step"
        );
    }
    if let Err(err) = write_password(client, flavor, remote_id, &crate::random_password()).await {
        tracing::error!(
            code = err.code(),
            "a new account could not be given a random password after a failed create step"
        );
    }
}

/// Create an account (D17): serialized per instance; created, then
/// disabled with exactly the libraries, then (Emby) a twin check and the
/// password, enabled last; every step read back.
///
/// Jellyfin takes the password with the create (without one the account
/// would log in with an empty password); the account then exists enabled
/// with every library until the policy write right after it. Emby ignores
/// the password on create and creates the account active with an empty
/// password, so the password follows the disable.
pub async fn create_account(
    client: &ServiceClient,
    flavor: Flavor,
    new: &NewAccount<'_>,
) -> Result<CreateOutcome, AdapterError> {
    refuse_empty(new.password)?;
    let libraries = checked_libraries(client, flavor, new.libraries).await?;
    let _guard = client.serialize(CREATE_LOCK).await;
    let body = match flavor {
        Flavor::Jellyfin => {
            json!({ "Name": new.username, "Password": new.password.expose_secret() })
        }
        Flavor::Emby => json!({ "Name": new.username }),
    };
    let created: Value = match client
        .post_json_sensitive("Users/New", &body, flavor == Flavor::Jellyfin)
        .await
    {
        Ok(created) => created,
        // Both answer a taken name (any case) with 400.
        Err(AdapterError::Rejected(detail)) => {
            return match named(client, flavor, new.username).await?.first() {
                Some(taken) => Ok(CreateOutcome::NameTaken {
                    remote_id: taken.account.remote_id.clone(),
                }),
                None => Err(AdapterError::Rejected(detail)),
            };
        }
        Err(err) => return Err(err),
    };
    let remote_id = match Fetched::parse(created, flavor.lockout()) {
        Ok(created) => created.account.remote_id,
        // Created, but the answer is unreadable: find it by name and leave
        // it unusable instead of an unowned active account.
        Err(error) => {
            let found = named(client, flavor, new.username)
                .await
                .unwrap_or_default();
            let newest = found.iter().max_by(|a, b| a.created().cmp(&b.created()));
            let Some(newest) = newest else {
                return Err(error);
            };
            let remote_id = newest.account.remote_id.clone();
            leave_unusable(client, flavor, &remote_id).await;
            return Ok(CreateOutcome::Unfinished { remote_id, error });
        }
    };
    match finish_create(client, flavor, new, &remote_id, &libraries).await {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            leave_unusable(client, flavor, &remote_id).await;
            Ok(CreateOutcome::Unfinished { remote_id, error })
        }
    }
}

async fn finish_create(
    client: &ServiceClient,
    flavor: Flavor,
    new: &NewAccount<'_>,
    remote_id: &str,
    libraries: &[String],
) -> Result<CreateOutcome, AdapterError> {
    // Disabled with exactly the libraries, in one full-policy write.
    let current = fetch_writable(client, remote_id, flavor.lockout()).await?;
    let mut policy = current.into_policy()?;
    policy.insert("IsDisabled".to_owned(), Value::Bool(true));
    grant_policy(&mut policy, libraries);
    write_policy(client, remote_id, policy).await?;
    let after = fetch(client, remote_id, flavor.lockout()).await?;
    if after.account.enabled || !grants(&after.account, libraries) {
        return Err(read_back_differs("the new account's policy"));
    }
    if flavor == Flavor::Emby {
        // Twins (D17): the oldest account of a name wins logins; Starr's
        // younger one stays disabled with a password nobody knows.
        let mut twins = named(client, flavor, new.username).await?;
        twins.retain(|t| t.account.remote_id != remote_id);
        // A twin whose creation time cannot be compared counts as older.
        let mine = after.created();
        let older = twins
            .iter()
            .filter(|t| match (t.created(), mine) {
                (Some(theirs), Some(mine)) => theirs <= mine,
                _ => true,
            })
            .min_by(|a, b| a.created().cmp(&b.created()));
        if let Some(oldest) = older {
            write_password(client, flavor, remote_id, &crate::random_password()).await?;
            return Ok(CreateOutcome::NameTaken {
                remote_id: oldest.account.remote_id.clone(),
            });
        }
        write_password(client, flavor, remote_id, new.password).await?;
        password_read_back(client, flavor, remote_id).await?;
    }
    let account = set_enabled(client, flavor, remote_id, true).await?;
    Ok(CreateOutcome::Created(account))
}

/// Delete the account `plan` names (D16): refused for another account
/// than the plan names and for an administrator (read first); already
/// gone is done without a write. Read back: the service must answer the
/// account as not found afterwards.
pub async fn delete_account(
    client: &ServiceClient,
    flavor: Flavor,
    remote_id: &str,
    plan: &DeletePlan,
) -> Result<(), AdapterError> {
    plan.check(remote_id)?;
    match fetch_writable(client, remote_id, flavor.lockout()).await {
        Ok(_) => {}
        Err(AdapterError::NotFound(_)) => return Ok(()),
        Err(err) => return Err(err),
    }
    match client.delete(&user_path(remote_id)).await {
        // Gone between the read and the delete: done all the same.
        Ok(()) | Err(AdapterError::NotFound(_)) => {}
        Err(err) => return Err(err),
    }
    match fetch(client, remote_id, flavor.lockout()).await {
        Err(AdapterError::NotFound(_)) => Ok(()),
        Ok(_) => Err(read_back_differs("the account")),
        Err(err) => Err(err),
    }
}
