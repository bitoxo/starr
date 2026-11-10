//! The write half of `accounts.reconcile` (spec 0005 slice 4): execute the
//! executable steps of a plan the safe-mode gate let through
//! (`starr_core::provisioning::gate`), one member at a time. Every step
//! - re-checks right before writing that Starr still owns the account and
//!   it is not ignored (an admin may have changed that since the plan);
//! - goes through the adapter, which refuses administrators and lockouts
//!   and reads back after the write;
//! - stores the read-back in the inventory, records the result for "needs
//!   you" and writes an audit entry (`account.enable`, `account.disable`,
//!   `account.libraries`) with the outcome, in its own statement.
//!
//! A failed step stops that member's remaining steps; other members go on.
//! Every step is idempotent (the adapter writes nothing when the account is
//! already as wanted), so a retried job is safe.
//!
//! `accounts.password` (slice 5) runs a member's steps here too, with the
//! member's password from a handoff ([`WithPassword`]): then the password
//! step runs as well, in plan order (an account without a password is
//! disabled and limited first), after re-checking that the member is still
//! active (D9). It is audited as `account.password`, never with a value.
//! Before the write, the generation it writes is marked on Starr's record;
//! a retried job that finds the mark without a recorded result does not
//! write the same password again (devices would be signed out twice).
//!
//! Slice 6: a `Delete` step (due deletion order, D16) deletes the account
//! with the planner's `DeletePlan`; Starr then forgets it (`account.delete`).
//! Every executed destructive step is recorded for the rolling cap, and
//! every read-back after Starr's own write becomes the drift baseline.
//!
//! Spec 0006 slice 7: on a service without a disable flag (Navidrome) a
//! `Disable` step writes a random password (the adapter's `set_enabled`)
//! and Starr records the account as locked (`locked_by_starr`, D9); the
//! member's next password written unlocks it.

use chrono::Utc;
use secrecy::SecretString;
use serde_json::json;
use starr_core::accounts::{DisableMode, LibraryGrant, NonEmpty, RemoteAccount};
use starr_core::adapters::AdapterError;
use starr_core::members::MemberStatus;
use starr_core::provisioning::{self, Action, Plan, PlanEntry, Step, WritesMode};
use starr_db::account_writes::WriteResult;
use starr_db::audit::{ActorKind, AuditEntry};
use starr_db::remote_accounts::ObservedAccount;
use starr_db::{Database, DbError};
use tokio_util::sync::CancellationToken;

use crate::JobError;
use crate::adapters::AnyAccounts;
use crate::services::{Failure, Openers, instance_client};

/// What executing one instance's plan did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Executed {
    /// Steps applied (or found already in effect) and read back.
    pub(crate) applied: u32,
    pub(crate) failed: u32,
    /// Steps not tried: an earlier step of the member failed, or ownership
    /// changed since the plan.
    pub(crate) skipped: u32,
    /// A failure says the service itself is in trouble (unreachable,
    /// throttled, key refused): the run does not count as a complete live
    /// run (M8 A).
    pub(crate) service_trouble: bool,
    /// An admin paused the writes during the run; the rest waits.
    pub(crate) interrupted: bool,
}

/// The member's password for the steps that need it (from a handoff).
pub(crate) struct WithPassword<'a> {
    pub(crate) password: &'a SecretString,
    /// The member's password generation it belongs to.
    pub(crate) generation: i64,
}

/// Why nothing could be executed at all.
pub(crate) enum NotExecuted {
    /// The instance cannot be opened (credential, removed).
    Unavailable(Failure),
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn observed(account: &RemoteAccount) -> ObservedAccount {
    ObservedAccount {
        remote_id: account.remote_id.clone(),
        username: account.username.clone(),
        enabled: account.enabled,
        privileged: account.privileged,
        locked_by_service: account.locked_by_service,
        libraries: match &account.libraries {
            LibraryGrant::All => None,
            LibraryGrant::Only(ids) => Some(ids.clone()),
        },
    }
}

const fn audit_action(action: &Action) -> &'static str {
    match action {
        Action::Create { .. } => "account.create",
        Action::Enable => "account.enable",
        Action::Disable => "account.disable",
        Action::SetLibraries { .. } => "account.libraries",
        Action::SetPassword => "account.password",
        Action::Delete => "account.delete",
    }
}

/// What one applied step left.
enum Applied {
    /// The account as read back after the write.
    Account(RemoteAccount),
    /// The password is written; the account as read back afterwards, if
    /// that worked.
    Password(Option<RemoteAccount>),
    /// Deleted in the service (read back as gone).
    Deleted,
}

pub(crate) const fn service_trouble(err: &AdapterError) -> bool {
    matches!(
        err,
        AdapterError::Transient(_)
            | AdapterError::Throttled { .. }
            | AdapterError::CredentialsInvalid
    )
}

async fn apply(
    adapter: &AnyAccounts,
    entry: &PlanEntry,
    remote_id: &str,
    step: &Step,
    password: Option<&WithPassword<'_>>,
) -> Result<Applied, AdapterError> {
    let account = match &step.action {
        Action::SetPassword => {
            let Some(with) = password else {
                return Err(AdapterError::Rejected(
                    "a password step needs the member's password; not executed by a plan".into(),
                ));
            };
            adapter.set_password(remote_id, with.password).await?;
            // Written: a failed read-back afterwards must not make a
            // later run write (and sign out) again.
            return Ok(Applied::Password(match adapter.account(remote_id).await {
                Ok(account) => Some(account),
                Err(err) => {
                    tracing::warn!(
                        code = err.code(),
                        "account not read back after the password write"
                    );
                    None
                }
            }));
        }
        Action::Enable => adapter.set_enabled(remote_id, true).await?,
        Action::Disable => adapter.set_enabled(remote_id, false).await?,
        Action::SetLibraries { libraries, .. } => {
            let libraries = NonEmpty::new(libraries.clone()).ok_or_else(|| {
                AdapterError::Rejected("the plan has no library list; never written empty".into())
            })?;
            adapter.set_libraries(remote_id, &libraries).await?
        }
        Action::Delete => {
            let plan = provisioning::delete_plan(entry, step).ok_or_else(|| {
                AdapterError::Rejected("no due deletion order plans this delete".into())
            })?;
            adapter.delete_account(remote_id, &plan).await?;
            return Ok(Applied::Deleted);
        }
        Action::Create { .. } => {
            return Err(AdapterError::Rejected(
                "creating needs the member's password; not executed by a plan".into(),
            ));
        }
    };
    Ok(Applied::Account(account))
}

/// Whether a password write of `generation` was started earlier without a
/// recorded result (a crash or a cancelled job in between): it may have
/// reached the service, so it is not written again.
async fn interrupted_password_write(
    db: &Database,
    instance: i64,
    member_id: i64,
    generation: i64,
) -> Result<bool, DbError> {
    Ok(db
        .member_service_accounts(member_id)
        .await?
        .iter()
        .any(|r| {
            r.service_instance_id == instance
                && r.password_attempt == Some(generation)
                && r.password_written != Some(generation)
        }))
}

/// Audit one write with its outcome; a failed audit write is logged, never
/// undoes or repeats the write.
pub(crate) async fn audit(db: &Database, target: &str, step: &Step, details: serde_json::Value) {
    let details = details.to_string();
    let entry = AuditEntry {
        actor_kind: ActorKind::System,
        actor_name: None,
        action: audit_action(&step.action),
        target: Some(target),
        details: Some(&details),
    };
    if let Err(err) = db.audit(&entry, Utc::now()).await {
        tracing::error!(target, error = %err, "account write done but audit entry not written");
    }
}

/// Whether Starr still owns `remote_id` for `member_id` and it is not
/// ignored (checked right before writing).
async fn still_owned(
    db: &Database,
    instance: i64,
    member_id: i64,
    remote_id: &str,
) -> Result<bool, DbError> {
    if db.account_owner(instance, remote_id).await? != Some(member_id) {
        return Ok(false);
    }
    Ok(db
        .remote_accounts(instance)
        .await?
        .iter()
        .any(|a| a.remote_id == remote_id && a.ignored_at.is_none() && !a.privileged))
}

/// The accounts adapter of instance `instance`.
pub(crate) async fn open(
    db: &Database,
    openers: Openers<'_>,
    instance: i64,
) -> Result<Result<AnyAccounts, NotExecuted>, JobError> {
    let Some(conn) = db.service_connection(instance).await? else {
        return Ok(Err(NotExecuted::Unavailable(Failure::starr(
            "not_found",
            "removed before the run".to_owned(),
        ))));
    };
    let client = match instance_client(openers, &conn) {
        Ok((_, client)) => client,
        Err(failure) => return Ok(Err(NotExecuted::Unavailable(failure))),
    };
    Ok(AnyAccounts::new(&conn.adapter_type, client).ok_or_else(|| {
        NotExecuted::Unavailable(Failure::starr(
            "unsupported",
            "this service type has no accounts".to_owned(),
        ))
    }))
}

/// Execute the executable steps of `plan` on instance `instance`
/// (`service_name` for audit targets); `usernames` names the members.
pub(crate) async fn execute(
    db: &Database,
    openers: Openers<'_>,
    cancel: &CancellationToken,
    instance: i64,
    service_name: &str,
    plan: &Plan,
    username: impl Fn(i64) -> Option<String>,
) -> Result<Result<Executed, NotExecuted>, JobError> {
    let adapter = match open(db, openers, instance).await? {
        Ok(adapter) => adapter,
        Err(not) => return Ok(Err(not)),
    };
    let mut done = Executed::default();
    for (i, entry) in plan.entries.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(JobError::Cancelled);
        }
        // An admin may pause while the run is under way: the remaining
        // members wait (a step under way when the pause lands finishes).
        if entry.steps.iter().any(|s| s.action.executable())
            && db
                .account_writes(instance)
                .await?
                .is_none_or(|w| w.mode() != WritesMode::Live)
        {
            let rest = plan.entries.get(i..).unwrap_or_default();
            done.skipped += count(
                rest.iter()
                    .flat_map(|e| &e.steps)
                    .filter(|s| s.action.executable())
                    .count(),
            );
            done.interrupted = true;
            break;
        }
        let name = username(entry.member_id);
        execute_entry(
            db,
            &adapter,
            cancel,
            Target {
                instance,
                service_name,
                username: name.as_deref(),
            },
            entry,
            None,
            &mut done,
        )
        .await?;
    }
    Ok(Ok(done))
}

/// Where a member's steps run: the instance and the names for audit.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Target<'a> {
    pub(crate) instance: i64,
    pub(crate) service_name: &'a str,
    /// The member's username; `None`: the member is gone.
    pub(crate) username: Option<&'a str>,
}

async fn member_active(db: &Database, member_id: i64) -> Result<bool, DbError> {
    Ok(db
        .get_member(member_id)
        .await?
        .is_some_and(|m| m.status == MemberStatus::Active))
}

/// The account a member's steps run on.
struct Owned<'a> {
    instance: i64,
    member_id: i64,
    remote_id: &'a str,
}

/// What an audit entry of `step` names (never a secret).
fn step_details(instance: i64, remote_id: &str, step: &Step) -> serde_json::Value {
    let mut details = json!({
        "serviceId": instance,
        "remoteId": remote_id,
        "reason": step.reason,
    });
    if let Action::SetLibraries { libraries, .. } = &step.action {
        details["libraries"] = json!(libraries);
    }
    details
}

/// Whether a password step may write now.
enum PasswordCheck {
    Write,
    /// The member is not active (D9).
    Inactive,
    /// The account is not the member's any more.
    NotOwned,
    /// A write of this generation was started without a recorded result:
    /// not repeated (recorded as a failure for "needs you").
    Interrupted,
}

/// Check a password step right before it writes and mark the attempt.
async fn password_check(
    db: &Database,
    account: &Owned<'_>,
    generation: i64,
    now: chrono::DateTime<Utc>,
) -> Result<PasswordCheck, DbError> {
    if !member_active(db, account.member_id).await? {
        return Ok(PasswordCheck::Inactive);
    }
    if interrupted_password_write(db, account.instance, account.member_id, generation).await? {
        let detail = "a password write was interrupted before its result was recorded; \
                      not written again (the member enters the password once more)";
        let failed = WriteResult::Failed {
            code: "unknown",
            detail,
        };
        db.record_account_write(account.instance, account.member_id, failed, now)
            .await?;
        // Only the automatic repeat is refused: the member's next entry
        // (a new handoff, even of the same generation) may write again.
        db.clear_password_attempt(account.instance, account.member_id)
            .await?;
        return Ok(PasswordCheck::Interrupted);
    }
    let owned = db
        .begin_password_attempt(
            account.instance,
            account.member_id,
            account.remote_id,
            generation,
        )
        .await?;
    // Relinked by an admin since the ownership check: never written.
    Ok(if owned {
        PasswordCheck::Write
    } else {
        PasswordCheck::NotOwned
    })
}

/// Record a failed step for "needs you"; a failed password write's attempt
/// mark goes (its result is known, a later handoff may try again).
async fn record_failure(
    db: &Database,
    account: &Owned<'_>,
    password: bool,
    err: &AdapterError,
    now: chrono::DateTime<Utc>,
) -> Result<(), DbError> {
    if password {
        db.clear_password_attempt(account.instance, account.member_id)
            .await?;
    }
    let detail = err.to_string();
    let failed = WriteResult::Failed {
        code: err.code(),
        detail: &detail,
    };
    db.record_account_write(account.instance, account.member_id, failed, now)
        .await
}

/// Store what an applied step left: the read-back (also as the drift
/// baseline after Starr's own policy write), or forget a deleted account.
/// Returns whether the account is enabled now, where read back.
async fn record_applied(
    db: &Database,
    account: &Owned<'_>,
    applied: Applied,
    now: chrono::DateTime<Utc>,
) -> Result<Option<bool>, DbError> {
    let Owned {
        instance,
        member_id,
        remote_id,
    } = *account;
    match applied {
        Applied::Account(read) => {
            db.store_read_back(instance, &observed(&read), now).await?;
            let libraries = match &read.libraries {
                LibraryGrant::All => None,
                LibraryGrant::Only(ids) => Some(ids.as_slice()),
            };
            db.record_expected(instance, member_id, remote_id, read.enabled, libraries)
                .await?;
            Ok(Some(read.enabled))
        }
        Applied::Password(read) => match read {
            Some(read) => {
                db.store_read_back(instance, &observed(&read), now).await?;
                Ok(Some(read.enabled))
            }
            None => Ok(None),
        },
        Applied::Deleted => {
            db.forget_deleted_account(instance, member_id, remote_id)
                .await?;
            Ok(None)
        }
    }
}

/// No disable flag (D9): a `Disable` is a random password, and only
/// Starr's record says the account is locked. Recorded before the write;
/// whether the step is one.
async fn record_scramble(
    db: &Database,
    adapter: &AnyAccounts,
    account: &Owned<'_>,
    step: &Step,
) -> Result<bool, DbError> {
    let scrambled = matches!(step.action, Action::Disable)
        && adapter.rules().disable == DisableMode::ScramblePassword;
    if scrambled {
        db.record_locked_by_starr(
            account.instance,
            account.member_id,
            account.remote_id,
            Utc::now(),
        )
        .await?;
    }
    Ok(scrambled)
}

/// Run one member's steps on an owned account: the executable ones, and
/// with `password` also the password step (never a create: that has no
/// account yet, see `passwords`).
pub(crate) async fn execute_entry(
    db: &Database,
    adapter: &AnyAccounts,
    cancel: &CancellationToken,
    at: Target<'_>,
    entry: &PlanEntry,
    password: Option<&WithPassword<'_>>,
    done: &mut Executed,
) -> Result<(), JobError> {
    let Target {
        instance,
        service_name,
        username,
    } = at;
    let steps: Vec<&Step> = entry
        .steps
        .iter()
        .filter(|s| {
            s.action.executable() || (password.is_some() && matches!(s.action, Action::SetPassword))
        })
        .collect();
    if steps.is_empty() {
        return Ok(());
    }
    let (Some(remote_id), Some(name)) = (entry.remote_id.as_deref(), username) else {
        done.skipped += count(steps.len());
        return Ok(());
    };
    let target = format!("{service_name}:{name}");
    if !still_owned(db, instance, entry.member_id, remote_id).await? {
        tracing::info!(target, "ownership changed since the plan; steps skipped");
        done.skipped += count(steps.len());
        return Ok(());
    }
    for (i, step) in steps.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(JobError::Cancelled);
        }
        let now = Utc::now();
        let mut details = step_details(instance, remote_id, step);
        let account = Owned {
            instance,
            member_id: entry.member_id,
            remote_id,
        };
        let writes_password = match (&step.action, password) {
            (Action::SetPassword, Some(with)) => Some(with.generation),
            _ => None,
        };
        if let Some(generation) = writes_password {
            match password_check(db, &account, generation, now).await? {
                PasswordCheck::Write => {}
                // D9: never a password for a member who is not active
                // (checked right before the write).
                PasswordCheck::Inactive | PasswordCheck::NotOwned => {
                    done.skipped += count(steps.len() - i);
                    return Ok(());
                }
                PasswordCheck::Interrupted => {
                    tracing::warn!(target, "password write interrupted earlier; not repeated");
                    details["result"] = json!("interrupted");
                    audit(db, &target, step, details).await;
                    done.failed += 1;
                    done.skipped += count(steps.len() - i - 1);
                    return Ok(());
                }
            }
        }
        // Recorded before the scramble is sent: a lock without a scramble
        // only asks the member for their password once more, a scramble
        // without the lock (a crash, a failed read-back) would leave an
        // account nobody can use that Starr shows as ready.
        let scrambled = record_scramble(db, adapter, &account, step).await?;
        match apply(adapter, entry, remote_id, step, password).await {
            Ok(applied) => {
                // The password first: a crash after this point must not
                // write it again.
                if let Some(generation) = writes_password {
                    db.record_password_written(instance, entry.member_id, remote_id, generation)
                        .await?;
                }
                if step.destructive() {
                    db.record_destructive_writes(instance, 1, now).await?;
                }
                details["result"] = json!("ok");
                let deleted = matches!(applied, Applied::Deleted);
                if let Some(enabled) = record_applied(db, &account, applied, now).await? {
                    details["enabled"] = json!(enabled && !scrambled);
                }
                if scrambled {
                    details["lockedByStarr"] = json!(true);
                }
                audit(db, &target, step, details).await;
                done.applied += 1;
                if deleted {
                    // Nothing is left to write for this member here.
                    done.skipped += count(steps.len() - i - 1);
                    return Ok(());
                }
            }
            Err(err) => {
                record_failure(db, &account, writes_password.is_some(), &err, now).await?;
                tracing::warn!(target, code = err.code(), error = %err, "account write failed");
                details["result"] = json!("failed");
                details["code"] = json!(err.code());
                audit(db, &target, step, details).await;
                done.failed += 1;
                done.service_trouble |= service_trouble(&err);
                done.skipped += count(steps.len() - i - 1);
                return Ok(());
            }
        }
    }
    db.record_account_write(instance, entry.member_id, WriteResult::Ok, Utc::now())
        .await?;
    Ok(())
}
