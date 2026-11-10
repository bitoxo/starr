//! The `Accounts` capability (ADR 0009, ADR 0011, spec 0005): accounts in a
//! consume service (Jellyfin, Emby; Navidrome since spec 0006 slice 7).
//!
//! The read half shipped with spec 0005 slice 1; the write half (create,
//! enable/disable, libraries, password) with slice 4; deletion
//! (`delete_account` with a [`DeletePlan`] from a due deletion order, D16)
//! with slice 6.
//!
//! Rules every implementation follows:
//! - a listing is `complete` only with proof that it holds every account
//!   (lesson 8); entries Starr cannot read safely (no ID, no name, no
//!   policy) are left out and make it incomplete;
//! - `privileged` errs on the safe side: an account whose rights cannot be
//!   read is never reported as unprivileged;
//! - every write reads the account first and refuses an administrator with
//!   [`AdapterError::Rejected`] before writing anything (D2);
//! - every write reads back and returns what the service then reports
//!   (lesson 9); a read-back that differs from the intent is an error;
//! - policy writes are read–modify–write of the full object: only the
//!   fields Starr owns change (enabled, library list), never others (D11);
//! - a lockout set by the service is never lifted (D10);
//! - library lists are never empty and only name libraries the service
//!   lists (D11); passwords are never empty (D12);
//! - an account is deleted only with a [`DeletePlan`], which only the
//!   planner hands out for a step of a due deletion order (lesson 27,
//!   type-level), and only for the account it names.

use std::collections::{HashMap, HashSet};
use std::future::Future;

use secrecy::SecretString;

use crate::adapters::{AdapterError, Listing, Probe};

/// Which libraries an account may see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryGrant {
    /// Every library, including ones created later.
    All,
    /// Only these libraries (the service's grant IDs; Emby: the library
    /// `Guid`, Jellyfin: the library `ItemId`).
    Only(Vec<String>),
}

/// One account as the service reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAccount {
    /// The service's ID of the account; Starr's ownership key (D4).
    pub remote_id: String,
    /// The name there (may differ in case from Starr's username).
    pub username: String,
    /// The service's enabled flag. A Jellyfin lockout disables the
    /// account, so a locked account is also not enabled; see
    /// `locked_by_service`. Always `true` without a flag
    /// ([`DisableMode::ScramblePassword`]): there, only Starr knows whether
    /// it scrambled the password.
    pub enabled: bool,
    /// Administrator in the service: Starr never touches it (D2).
    pub privileged: bool,
    /// Locked by the service after failed logins (D10): its own state,
    /// never lifted by Starr. `false` where the service does not expose a
    /// current lock (Emby).
    pub locked_by_service: bool,
    pub libraries: LibraryGrant,
}

/// A list with at least one element (a library grant is never empty, D11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonEmpty<T>(Vec<T>);

impl<T> NonEmpty<T> {
    /// `None` for an empty list.
    #[must_use]
    pub fn new(items: Vec<T>) -> Option<Self> {
        (!items.is_empty()).then_some(Self(items))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }
}

/// How a service disables an account (D9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisableMode {
    /// A flag on the account (Jellyfin, Emby `IsDisabled`).
    Flag,
    /// No flag: a random password that is never stored (Navidrome).
    ScramblePassword,
}

/// How finely a service grants libraries (lesson 1: a required field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisibilityGranularity {
    PerLibrary,
    AllOnly,
}

/// How a service compares usernames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameMatching {
    CaseInsensitive,
    /// Logins are case-insensitive, creation is not (Audiobookshelf).
    CaseSensitiveCreate,
}

/// What a service's accounts can do (static per adapter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountRules {
    pub disable: DisableMode,
    /// The adapter can delete accounts (with a [`DeletePlan`], D16).
    pub delete: bool,
    pub visibility: VisibilityGranularity,
    pub username_matching: NameMatching,
    /// Setting a password signs out every device of the account (shown
    /// before a member switches, M2).
    pub password_change_signs_out: bool,
}

/// An account Starr creates.
#[derive(Debug, Clone, Copy)]
pub struct NewAccount<'a> {
    /// Starr's username (D18).
    pub username: &'a str,
    pub password: &'a SecretString,
    pub libraries: &'a NonEmpty<String>,
}

/// What creating an account led to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateOutcome {
    /// Created, enabled last, read back.
    Created(RemoteAccount),
    /// The name is taken by this account (an outcome, not an error). For
    /// a twin (Emby creates duplicates concurrently, D17) this is the
    /// oldest account of that name; Starr's younger twin stays disabled
    /// with a random password nobody knows.
    NameTaken { remote_id: String },
    /// The account was created, but a later step failed (`error`). The
    /// adapter left it as safe as it could: disabled, and where that write
    /// failed too and the service creates accounts with an empty password
    /// (Emby), with a random password nobody knows. Starr owns it from now
    /// on and keeps it disabled until the member's password is written.
    Unfinished {
        remote_id: String,
        error: AdapterError,
    },
}

/// The permission to delete one account (D16, lesson 27): only
/// `provisioning::delete_plan` makes one, and only for a `Delete` step with
/// reason `DeletionDue` that the plan entry itself carries, for the account
/// the entry owns. Starr's only caller passes entries of the plan the
/// planner just computed; the type keeps adapters from being handed a bare
/// remote ID to delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletePlan {
    remote_id: String,
}

impl DeletePlan {
    /// Only the planner's `provisioning::delete_plan` makes one.
    pub(crate) const fn new(remote_id: String) -> Self {
        Self { remote_id }
    }

    /// The one account this plan allows deleting.
    #[must_use]
    pub fn remote_id(&self) -> &str {
        &self.remote_id
    }

    /// Refuse a delete of any other account than the plan names.
    pub fn check(&self, remote_id: &str) -> Result<(), AdapterError> {
        if self.remote_id == remote_id {
            Ok(())
        } else {
            Err(AdapterError::Rejected(
                "the deletion plan names another account; nothing deleted".to_owned(),
            ))
        }
    }
}

/// A consume service whose accounts Starr reads and writes.
pub trait Accounts: Probe {
    fn account_rules(&self) -> &'static AccountRules;

    /// Every account of the service, with completeness evidence.
    fn accounts(&self)
    -> impl Future<Output = Result<Listing<RemoteAccount>, AdapterError>> + Send;

    /// One account by remote ID; [`AdapterError::NotFound`] if the service
    /// has none with that ID.
    fn account(
        &self,
        remote_id: &str,
    ) -> impl Future<Output = Result<RemoteAccount, AdapterError>> + Send;

    /// Create an account: disabled with the password and libraries first,
    /// enabled last (Emby creates accounts active with an empty password).
    /// Refuses an empty password. Creates are serialized per instance.
    fn create_account(
        &self,
        new: &NewAccount<'_>,
    ) -> impl Future<Output = Result<CreateOutcome, AdapterError>> + Send;

    /// Enable or disable the account. Enabling refuses an account locked by
    /// the service (D10). Already in that state: nothing is written.
    ///
    /// [`DisableMode::ScramblePassword`] (no flag, D9): disabling writes a
    /// random password nobody knows, every time (the service cannot tell
    /// whether Starr did so before; Starr records it as `locked_by_starr`
    /// and the planner never disables a locked account again); enabling is
    /// refused without a request, since only the member's password
    /// ([`Self::set_password`]) unlocks the account.
    fn set_enabled(
        &self,
        remote_id: &str,
        enabled: bool,
    ) -> impl Future<Output = Result<RemoteAccount, AdapterError>> + Send;

    /// Grant exactly these libraries (never "all libraries"). Refuses IDs
    /// the service does not list. Already granted: nothing is written.
    fn set_libraries(
        &self,
        remote_id: &str,
        libraries: &NonEmpty<String>,
    ) -> impl Future<Output = Result<RemoteAccount, AdapterError>> + Send;

    /// Set the password (signs out the account's devices where
    /// `password_change_signs_out`). Refuses an empty password.
    fn set_password(
        &self,
        remote_id: &str,
        password: &SecretString,
    ) -> impl Future<Output = Result<(), AdapterError>> + Send;

    /// Delete the account `plan` names (D16). Reads it first and refuses an
    /// administrator (D2) or another account than the plan names; reads
    /// back that it is gone. Already gone: nothing is written.
    fn delete_account(
        &self,
        remote_id: &str,
        plan: &DeletePlan,
    ) -> impl Future<Output = Result<(), AdapterError>> + Send;
}

/// Complete listings in a row that must miss an account before it counts as
/// gone (D7, lesson 8).
pub const GONE_AFTER_COMPLETE_RUNS: u32 = 2;

/// What Starr knew about one account before a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownAccount {
    pub remote_id: String,
    /// Complete listings in a row that did not contain the account.
    pub missing_runs: u32,
    pub gone: bool,
}

/// What a scan decided about one account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// Listed for the first time.
    Appeared,
    /// Listed again after it was gone.
    Returned,
    /// Listed, and present before.
    Present,
    /// Missing from a complete listing, not often enough to be gone.
    Missing { missing_runs: u32 },
    /// Missing from enough complete listings: gone from now on.
    Gone { missing_runs: u32 },
}

/// Decide presence for every listed account and every known one that is not
/// gone. An incomplete listing only adds positive evidence: it never counts
/// towards absence. Known accounts that stay as they are (gone and still not
/// listed, or not listed by an incomplete listing) get no decision.
#[must_use]
pub fn presence(
    known: &[KnownAccount],
    listing: &Listing<RemoteAccount>,
) -> Vec<(String, Presence)> {
    let before: HashMap<&str, &KnownAccount> =
        known.iter().map(|k| (k.remote_id.as_str(), k)).collect();
    let mut listed: HashSet<&str> = HashSet::new();
    let mut decisions = Vec::new();
    for account in &listing.items {
        if !listed.insert(account.remote_id.as_str()) {
            continue;
        }
        let presence = match before.get(account.remote_id.as_str()) {
            None => Presence::Appeared,
            Some(k) if k.gone => Presence::Returned,
            Some(_) => Presence::Present,
        };
        decisions.push((account.remote_id.clone(), presence));
    }
    if listing.complete {
        for k in known {
            if k.gone || listed.contains(k.remote_id.as_str()) {
                continue;
            }
            let missing_runs = k.missing_runs.saturating_add(1);
            let presence = if missing_runs >= GONE_AFTER_COMPLETE_RUNS {
                Presence::Gone { missing_runs }
            } else {
                Presence::Missing { missing_runs }
            };
            decisions.push((k.remote_id.clone(), presence));
        }
    }
    decisions
}

/// The member a foreign account is suggested for (M3): the one whose
/// username equals the account's name case-insensitively. Privileged
/// accounts are never offered; a suggestion is never a link.
#[must_use]
pub fn suggested_member<'a, M>(
    account_name: &str,
    privileged: bool,
    members: &'a [M],
    username: impl Fn(&M) -> &str,
) -> Option<&'a M> {
    if privileged {
        return None;
    }
    let wanted = account_name.trim().to_lowercase();
    members
        .iter()
        .find(|m| username(m).to_lowercase() == wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(id: &str) -> RemoteAccount {
        RemoteAccount {
            remote_id: id.to_owned(),
            username: id.to_owned(),
            enabled: true,
            privileged: false,
            locked_by_service: false,
            libraries: LibraryGrant::All,
        }
    }

    fn known(id: &str, missing_runs: u32, gone: bool) -> KnownAccount {
        KnownAccount {
            remote_id: id.to_owned(),
            missing_runs,
            gone,
        }
    }

    fn listing(ids: &[&str], complete: bool) -> Listing<RemoteAccount> {
        Listing {
            items: ids.iter().map(|id| account(id)).collect(),
            complete,
        }
    }

    #[test]
    fn listed_accounts_appear_stay_or_return() {
        let known = [known("a", 0, false), known("g", 2, true)];
        let got = presence(&known, &listing(&["a", "b", "g", "b"], true));
        assert_eq!(
            got,
            vec![
                ("a".to_owned(), Presence::Present),
                ("b".to_owned(), Presence::Appeared),
                ("g".to_owned(), Presence::Returned),
            ]
        );
    }

    #[test]
    fn an_account_goes_only_after_two_complete_listings() {
        let first = presence(&[known("a", 0, false)], &listing(&[], true));
        assert_eq!(
            first,
            vec![("a".to_owned(), Presence::Missing { missing_runs: 1 })]
        );
        let second = presence(&[known("a", 1, false)], &listing(&[], true));
        assert_eq!(
            second,
            vec![("a".to_owned(), Presence::Gone { missing_runs: 2 })]
        );
        // Gone and still missing: nothing to decide.
        assert_eq!(
            presence(&[known("a", 2, true)], &listing(&[], true)),
            vec![]
        );
    }

    #[test]
    fn an_incomplete_listing_never_counts_towards_absence() {
        let got = presence(
            &[known("a", 1, false), known("b", 0, false)],
            &listing(&["b"], false),
        );
        assert_eq!(got, vec![("b".to_owned(), Presence::Present)]);
    }

    #[test]
    fn suggestions_match_names_case_insensitively_and_skip_privileged() {
        fn name<'a>(m: &'a (i64, &str)) -> &'a str {
            m.1
        }
        let members = [(1, "alex"), (2, "sam")];
        assert_eq!(
            suggested_member("Alex", false, &members, name).map(|m| m.0),
            Some(1)
        );
        assert_eq!(
            suggested_member(" SAM ", false, &members, name).map(|m| m.0),
            Some(2)
        );
        assert_eq!(suggested_member("Alex", true, &members, name), None);
        assert_eq!(suggested_member("alexa", false, &members, name), None);
    }
}
