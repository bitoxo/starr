//! Navidrome's [`Accounts`] (spec 0006 slice 7, Q9 A; spec 0005 D2, D9,
//! D11, D16, D17): users through the native API with the admin session.
//! Evidence: `docs/concept/capabilities.md`, "Navidrome" (traps); shapes
//! from Navidrome's source (`model/user.go`, `server/nativeapi`), not
//! recorded yet.
//!
//! Navidrome has **no disable flag** ([`DisableMode::ScramblePassword`]):
//! disabling writes a random password nobody knows (Starr records the lock,
//! D9), and only the member's password unlocks the account again; enabling
//! is refused. Every account reads as enabled.
//!
//! Requests sent (and nothing else):
//! - `GET /api/user`, `GET /api/user/{id}`, `GET /api/user/{id}/library`
//!   (only where a user object lacks its libraries), `GET /api/library`;
//! - `PUT /api/user/{id}` with the **full** user object as read plus
//!   `password` (a partial body empties the username: the trap), for a
//!   member's password or a random one (disable, unfinished create);
//! - `PUT /api/user/{id}/library {"libraryIds": [...]}`;
//! - `POST /api/user {userName, name, password, isAdmin: false}` with a
//!   random password (create, serialized per instance);
//! - `DELETE /api/user/{id}` (only with a `DeletePlan` for that account).
//!
//! Every write reads the user first and refuses an administrator (D2:
//! Starr's own admin account and the operator's) before writing anything,
//! and reads back afterwards (lesson 9), including that the username
//! survived. Navidrome has no `ETag`: a change made between Starr's read
//! and its write is overwritten; the read is taken right before each write.

use std::collections::{BTreeSet, HashSet};

use chrono::{DateTime, FixedOffset};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use starr_adapter_kit::random_password;
use starr_core::accounts::{
    AccountRules, Accounts, CreateOutcome, DeletePlan, DisableMode, LibraryGrant, NameMatching,
    NewAccount, NonEmpty, RemoteAccount, VisibilityGranularity,
};
use starr_core::adapters::{AdapterError, Listing};
use starr_core::library::encode_component;

use crate::{Navidrome, WireLibrary};

/// What Navidrome's accounts can do.
pub const ACCOUNT_RULES: AccountRules = AccountRules {
    disable: DisableMode::ScramblePassword,
    delete: true,
    visibility: VisibilityGranularity::PerLibrary,
    username_matching: NameMatching::CaseInsensitive,
    // A password change bumps the user's token epoch: every session and
    // app login of the account ends (`capabilities.md`).
    password_change_signs_out: true,
};

/// Serializes creates per instance (D17: concurrent creates make twins,
/// Navidrome checks uniqueness in app code only).
const CREATE_LOCK: &str = "accounts.create";

/// The part of a user object Starr reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireUser {
    #[serde(default)]
    id: String,
    #[serde(default)]
    user_name: String,
    /// Missing reads as an administrator (safe side, D2).
    #[serde(default)]
    is_admin: Option<bool>,
    #[serde(default)]
    created_at: Option<String>,
    /// Since 0.58.0; absent in a shape Starr has not seen, then read from
    /// `GET /api/user/{id}/library`.
    #[serde(default)]
    libraries: Option<Vec<WireLibrary>>,
}

/// One user as the service returned it: the full object (for the full
/// PUT) and what Starr reads from it.
struct Fetched {
    raw: Map<String, Value>,
    user: WireUser,
}

impl Fetched {
    fn parse(raw: Value) -> Result<Self, AdapterError> {
        let user = WireUser::deserialize(&raw)
            .map_err(|err| AdapterError::Unknown(format!("user is not readable: {err}")))?;
        let Value::Object(raw) = raw else {
            return Err(AdapterError::Unknown("user is not an object".to_owned()));
        };
        Ok(Self { raw, user })
    }

    fn privileged(&self) -> bool {
        self.user.is_admin != Some(false)
    }

    fn created(&self) -> Option<DateTime<FixedOffset>> {
        self.user
            .created_at
            .as_deref()
            .and_then(|c| DateTime::parse_from_rfc3339(c).ok())
    }
}

fn user_path(remote_id: &str) -> String {
    format!("api/user/{}", encode_component(remote_id))
}

fn library_path(remote_id: &str) -> String {
    format!("{}/library", user_path(remote_id))
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

fn read_back_differs(what: &str) -> AdapterError {
    AdapterError::Unknown(format!(
        "read-back after the write shows {what} not as written; the service did not apply it"
    ))
}

fn refuse_empty(password: &SecretString) -> Result<(), AdapterError> {
    if password.expose_secret().is_empty() {
        return Err(AdapterError::Rejected(
            "an empty password is never written".to_owned(),
        ));
    }
    Ok(())
}

fn grant_ids(libraries: &[WireLibrary]) -> Vec<String> {
    libraries.iter().map(|l| l.id.to_string()).collect()
}

fn grants(account: &RemoteAccount, wanted: &[u64]) -> bool {
    match &account.libraries {
        LibraryGrant::All => false,
        LibraryGrant::Only(ids) => {
            ids.iter().cloned().collect::<BTreeSet<_>>()
                == wanted.iter().map(u64::to_string).collect::<BTreeSet<_>>()
        }
    }
}

impl Navidrome {
    /// The account of `fetched`; `None` if it cannot be read safely (no ID,
    /// no name). Libraries come from the object or, where it lacks them,
    /// from `GET /api/user/{id}/library`. Administrators see every library.
    async fn remote(&self, fetched: &Fetched) -> Result<Option<RemoteAccount>, AdapterError> {
        let user = &fetched.user;
        if user.id.trim().is_empty() || user.user_name.trim().is_empty() {
            return Ok(None);
        }
        let privileged = fetched.privileged();
        let libraries = if privileged {
            LibraryGrant::All
        } else if let Some(libraries) = &user.libraries {
            LibraryGrant::Only(grant_ids(libraries))
        } else {
            let (libraries, _): (Vec<WireLibrary>, _) =
                self.get(&library_path(&user.id), &[]).await?;
            LibraryGrant::Only(grant_ids(&libraries))
        };
        Ok(Some(RemoteAccount {
            remote_id: user.id.clone(),
            username: user.user_name.clone(),
            // No disable flag: only Starr knows whether it scrambled it.
            enabled: true,
            privileged,
            // The Subsonic throttle after failed logins is per IP and
            // passes by itself; Navidrome keeps no lock on the account.
            locked_by_service: false,
            libraries,
        }))
    }

    async fn fetch(&self, remote_id: &str) -> Result<Fetched, AdapterError> {
        let (raw, _): (Value, _) = self.get(&user_path(remote_id), &[]).await?;
        let fetched = Fetched::parse(raw)?;
        if fetched.user.id != remote_id {
            return Err(AdapterError::Unknown(
                "the service answered with another account".to_owned(),
            ));
        }
        Ok(fetched)
    }

    async fn fetch_account(
        &self,
        remote_id: &str,
    ) -> Result<(Fetched, RemoteAccount), AdapterError> {
        let fetched = self.fetch(remote_id).await?;
        let account = self.remote(&fetched).await?.ok_or_else(|| {
            AdapterError::Unknown("the account has no readable name there".to_owned())
        })?;
        Ok((fetched, account))
    }

    /// Read the account to write; an administrator is refused (D2).
    async fn fetch_writable(
        &self,
        remote_id: &str,
    ) -> Result<(Fetched, RemoteAccount), AdapterError> {
        let fetched = self.fetch(remote_id).await?;
        if fetched.privileged() {
            return Err(AdapterError::Rejected(
                "the account is an administrator in the service; Starr never touches it".to_owned(),
            ));
        }
        let account = self.remote(&fetched).await?.ok_or_else(|| {
            AdapterError::Unknown(
                "the account has no readable name there; Starr does not write it".to_owned(),
            )
        })?;
        Ok((fetched, account))
    }

    /// Write `password` with the full user object as read (the partial-PUT
    /// trap), then read back: same name, still no administrator.
    async fn write_password(
        &self,
        current: Fetched,
        password: &SecretString,
    ) -> Result<RemoteAccount, AdapterError> {
        let remote_id = current.user.id.clone();
        let username = current.user.user_name.clone();
        let mut body = current.raw;
        body.insert(
            "password".to_owned(),
            Value::String(password.expose_secret().to_owned()),
        );
        self.put(&user_path(&remote_id), &Value::Object(body), true)
            .await?;
        let after = self.fetch(&remote_id).await?;
        // No readable name any more: the partial-PUT trap struck.
        let Some(account) = self.remote(&after).await? else {
            return Err(read_back_differs("the username"));
        };
        if after.user.user_name != username {
            return Err(read_back_differs("the username"));
        }
        if account.privileged {
            return Err(read_back_differs("the account's rights"));
        }
        Ok(account)
    }

    /// The numeric IDs of `wanted`, all listed by the service; refuses an
    /// ID it does not list (D11). Duplicates are dropped.
    async fn checked_libraries(&self, wanted: &NonEmpty<String>) -> Result<Vec<u64>, AdapterError> {
        let (libraries, _): (Vec<WireLibrary>, _) = self.get("api/library", &[]).await?;
        let known: HashSet<u64> = libraries.iter().map(|l| l.id).collect();
        let mut out = Vec::with_capacity(wanted.as_slice().len());
        for id in wanted.as_slice() {
            let parsed = id.parse::<u64>().ok().filter(|i| known.contains(i));
            let Some(parsed) = parsed else {
                return Err(AdapterError::Rejected(format!(
                    "library {id:?} is not listed by the service (renamed or removed?)"
                )));
            };
            if !out.contains(&parsed) {
                out.push(parsed);
            }
        }
        Ok(out)
    }

    async fn grant(
        &self,
        remote_id: &str,
        libraries: &[u64],
    ) -> Result<RemoteAccount, AdapterError> {
        self.put(
            &library_path(remote_id),
            &json!({ "libraryIds": libraries }),
            false,
        )
        .await?;
        let (_, after) = self.fetch_account(remote_id).await?;
        if !grants(&after, libraries) {
            return Err(read_back_differs("the library list"));
        }
        Ok(after)
    }

    /// The accounts named `name` (any case), as listed.
    async fn named(&self, name: &str) -> Result<Vec<Fetched>, AdapterError> {
        let (users, _): (Vec<Value>, _) = self.get("api/user", &[]).await?;
        Ok(users
            .into_iter()
            .filter_map(|raw| Fetched::parse(raw).ok())
            .filter(|f| same_name(&f.user.user_name, name))
            .collect())
    }

    /// Best effort after a failed password step behind a create: a random
    /// password again, so the account never stays usable with a password
    /// that may or may not have landed. Starr keeps it locked.
    async fn leave_unusable(&self, remote_id: &str) {
        let result = match self.fetch_writable(remote_id).await {
            Ok((current, _)) => self.write_password(current, &random_password()).await,
            Err(err) => Err(err),
        };
        if let Err(err) = result {
            tracing::error!(
                code = err.code(),
                "a new Navidrome account could not be given a random password after a failed create step"
            );
        }
    }

    /// The rest of a create (D17 adapted, no enabled flag): twins, then
    /// exactly the libraries, then the member's password. `Err` carries
    /// whether the password write was under way.
    async fn finish_create(
        &self,
        new: &NewAccount<'_>,
        remote_id: &str,
        libraries: &[u64],
    ) -> Result<CreateOutcome, (AdapterError, bool)> {
        let mine = self.fetch(remote_id).await.map_err(|e| (e, false))?;
        // Twins (D17): the oldest account of a name wins; Starr's younger
        // one keeps the random password nobody knows.
        let mut twins = self.named(new.username).await.map_err(|e| (e, false))?;
        twins.retain(|t| t.user.id != remote_id);
        let created = mine.created();
        let older = twins
            .iter()
            .filter(|t| match (t.created(), created) {
                (Some(theirs), Some(mine)) => theirs <= mine,
                // Not comparable: counts as older.
                _ => true,
            })
            // The oldest dated twin; an undated one only when none is dated.
            .min_by(|a, b| match (a.created(), b.created()) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
        if let Some(oldest) = older {
            return Ok(CreateOutcome::NameTaken {
                remote_id: oldest.user.id.clone(),
            });
        }
        self.grant(remote_id, libraries)
            .await
            .map_err(|e| (e, false))?;
        let (current, _) = self
            .fetch_writable(remote_id)
            .await
            .map_err(|e| (e, false))?;
        let account = self
            .write_password(current, new.password)
            .await
            .map_err(|e| (e, true))?;
        if !grants(&account, libraries) {
            return Err((read_back_differs("the new account's libraries"), true));
        }
        Ok(CreateOutcome::Created(account))
    }
}

impl Accounts for Navidrome {
    fn account_rules(&self) -> &'static AccountRules {
        &ACCOUNT_RULES
    }

    /// `GET /api/user`, unpaged: complete unless the total disagrees, an ID
    /// repeats or a user cannot be read safely (no ID or no name, e.g. one
    /// a partial PUT emptied).
    async fn accounts(&self) -> Result<Listing<RemoteAccount>, AdapterError> {
        let (users, total): (Vec<Value>, _) = self.get("api/user", &[]).await?;
        let mut complete = total.is_some_and(|t| t == users.len() as u64);
        let mut seen = HashSet::new();
        let mut items = Vec::with_capacity(users.len());
        for raw in users {
            let Ok(fetched) = Fetched::parse(raw) else {
                complete = false;
                continue;
            };
            let account = match self.remote(&fetched).await {
                Ok(Some(account)) => account,
                Ok(None) => {
                    complete = false;
                    continue;
                }
                // Deleted between the listing and the library read.
                Err(AdapterError::NotFound(_)) => continue,
                Err(err) => return Err(err),
            };
            if !seen.insert(account.remote_id.clone()) {
                complete = false;
                continue;
            }
            items.push(account);
        }
        Ok(Listing { items, complete })
    }

    async fn account(&self, remote_id: &str) -> Result<RemoteAccount, AdapterError> {
        Ok(self.fetch_account(remote_id).await?.1)
    }

    /// Create (D17 adapted): serialized per instance; created with a random
    /// password nobody knows (Navidrome has no disabled state to create
    /// in), a twin check, exactly the libraries, the member's password
    /// last; every step read back.
    async fn create_account(&self, new: &NewAccount<'_>) -> Result<CreateOutcome, AdapterError> {
        refuse_empty(new.password)?;
        let libraries = self.checked_libraries(new.libraries).await?;
        let _guard = self.client.serialize(CREATE_LOCK).await;
        let scrambled = random_password();
        let body = json!({
            "userName": new.username,
            "name": new.username,
            "password": scrambled.expose_secret(),
            "isAdmin": false,
        });
        let created: Value = match self.post("api/user", &body).await {
            Ok(created) => created,
            // A taken name (any case) answers 400 `ra.validation.unique`.
            Err(AdapterError::Rejected(detail)) => {
                return match self.named(new.username).await?.first() {
                    Some(taken) => Ok(CreateOutcome::NameTaken {
                        remote_id: taken.user.id.clone(),
                    }),
                    None => Err(AdapterError::Rejected(detail)),
                };
            }
            Err(err) => return Err(err),
        };
        let remote_id = created
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned);
        let Some(remote_id) = remote_id else {
            // Created, but the answer is unreadable: the newest account of
            // the name is Starr's; it has the random password already.
            let error = AdapterError::Unknown("the create answer names no account".to_owned());
            // If the listing fails too, the new account stays unowned; it
            // has the random password, so nobody can use it, and the next
            // scan shows it as a foreign account with the member's name.
            let found = self.named(new.username).await.unwrap_or_default();
            return match found.iter().max_by_key(|f| f.created()) {
                Some(newest) => Ok(CreateOutcome::Unfinished {
                    remote_id: newest.user.id.clone(),
                    error,
                }),
                None => Err(error),
            };
        };
        match self.finish_create(new, &remote_id, &libraries).await {
            Ok(outcome) => Ok(outcome),
            Err((error, password_attempted)) => {
                if password_attempted {
                    self.leave_unusable(&remote_id).await;
                }
                Ok(CreateOutcome::Unfinished { remote_id, error })
            }
        }
    }

    /// No flag (D9): disabling writes a random password nobody knows, every
    /// time (the planner never disables an account Starr locked); enabling
    /// is refused without a request, only the member's password unlocks.
    async fn set_enabled(
        &self,
        remote_id: &str,
        enabled: bool,
    ) -> Result<RemoteAccount, AdapterError> {
        if enabled {
            return Err(AdapterError::Rejected(
                "Navidrome has no enabled flag; only the member's password unlocks the account"
                    .to_owned(),
            ));
        }
        let (current, _) = self.fetch_writable(remote_id).await?;
        self.write_password(current, &random_password()).await
    }

    /// `PUT /api/user/{id}/library` with exactly these IDs (never empty,
    /// only listed ones, D11). Already granted: nothing is written.
    async fn set_libraries(
        &self,
        remote_id: &str,
        libraries: &NonEmpty<String>,
    ) -> Result<RemoteAccount, AdapterError> {
        let wanted = self.checked_libraries(libraries).await?;
        let (_, current) = self.fetch_writable(remote_id).await?;
        if grants(&current, &wanted) {
            return Ok(current);
        }
        self.grant(remote_id, &wanted).await
    }

    /// The full user object with the password (signs out every session of
    /// the account).
    async fn set_password(
        &self,
        remote_id: &str,
        password: &SecretString,
    ) -> Result<(), AdapterError> {
        refuse_empty(password)?;
        let (current, _) = self.fetch_writable(remote_id).await?;
        self.write_password(current, password).await?;
        Ok(())
    }

    /// Delete the account `plan` names (D16): refused for another account
    /// and for an administrator (read first); already gone is done without
    /// a write; read back as not found.
    async fn delete_account(&self, remote_id: &str, plan: &DeletePlan) -> Result<(), AdapterError> {
        plan.check(remote_id)?;
        match self.fetch_writable(remote_id).await {
            Ok(_) => {}
            Err(AdapterError::NotFound(_)) => return Ok(()),
            Err(err) => return Err(err),
        }
        match self.delete(&user_path(remote_id)).await {
            Ok(()) | Err(AdapterError::NotFound(_)) => {}
            Err(err) => return Err(err),
        }
        match self.fetch(remote_id).await {
            Err(AdapterError::NotFound(_)) => Ok(()),
            Ok(_) => Err(read_back_differs("the account")),
            Err(err) => Err(err),
        }
    }
}
