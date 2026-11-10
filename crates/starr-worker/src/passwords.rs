//! `accounts.password` (spec 0005 slice 5, ADR 0010): write members'
//! passwords from their password handoffs.
//!
//! For every handoff in flight the job
//! - re-checks that the member is active and still at the handoff's
//!   password generation (a suspended member gets no password, D9; a newer
//!   password brings its own handoff); otherwise the handoff is deleted;
//! - opens it with the worker's key, bound to the member and generation;
//! - plans every consume instance with accounts afresh and, per instance
//!   where the member's entry has a step that needs the password, asks the
//!   same safe-mode gate as `accounts.reconcile`
//!   (`starr_core::provisioning::gate`): plan only, paused, incomplete or
//!   held means nothing is written and the handoff waits (at most its
//!   hour, then the account shows "activate" again);
//! - creates the account that waited only for a password (D17 order in the
//!   adapter, serialized per instance) or runs the member's steps on the
//!   owned account in plan order, the password step included;
//! - deletes the handoff when nothing is left that a later run could
//!   still do: written, refused, or failed for a reason a retry would not
//!   change. Trouble with the service itself (unreachable, throttled, key
//!   refused) or a gate that holds keeps it for a later run within the hour
//!   (queued by the next live plan run).
//!
//! The plaintext lives only in memory for the run (`SecretString`); it is
//! never logged, never in a report, an audit entry or an error.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use serde_json::json;
use starr_core::accounts::{CreateOutcome, DisableMode, NewAccount, NonEmpty, RemoteAccount};
use starr_core::adapters::AdapterError;
use starr_core::jobs::{Expectation, ItemOutcome, Report, ReportItem, Trigger};
use starr_core::members::MemberStatus;
use starr_core::provisioning::{Action, Decision, Gate, PlanEntry, Step};
use starr_db::Database;
use starr_db::DbError;
use starr_db::account_writes::WriteResult;
use starr_db::password_handoffs::StoredHandoff;
use starr_db::remote_accounts::ObservedAccount;
use tokio_util::sync::CancellationToken;

use crate::account_writes::{
    self, Executed, NotExecuted, Target, WithPassword, audit, service_trouble,
};
use crate::adapters::AnyAccounts;
use crate::provisioning::{Planned, enqueue_reconcile, gate_of, replan};
use crate::secrets::CredentialKey;
use crate::services::Openers;
use crate::{JobContext, JobError};

/// Rounds of handoffs one run takes (new ones arrive while it runs).
const MAX_ROUNDS: usize = 5;

/// What one member × instance unit came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    /// Created, or every step written and read back.
    Done,
    /// The gate holds (plan only, paused, incomplete, held): nothing
    /// written, the handoff waits.
    Waiting,
    /// Not tried: the decision or ownership changed since the plan, or the
    /// member is not active any more.
    Skipped,
    /// Failed; `retry`: trouble with the service, a later run may succeed.
    Failed { retry: bool },
}

/// Totals over one run.
#[derive(Default)]
struct Tally {
    done: u64,
    waiting: u64,
    skipped: u64,
    failed: u64,
    created: u64,
    passwords: u64,
    handoffs_used: u64,
    handoffs_dropped: u64,
    items: Vec<ReportItem>,
}

impl Tally {
    fn record(&mut self, target: String, unit: Unit) {
        let (outcome, code, detail) = match unit {
            Unit::Done => {
                self.done += 1;
                (ItemOutcome::Succeeded, None, "written")
            }
            Unit::Waiting => {
                self.waiting += 1;
                (
                    ItemOutcome::Skipped,
                    None,
                    "waits for the gate (writes off, paused or held)",
                )
            }
            Unit::Skipped => {
                self.skipped += 1;
                (ItemOutcome::Skipped, None, "changed since the plan")
            }
            Unit::Failed { retry } => {
                self.failed += 1;
                (
                    ItemOutcome::Failed,
                    Some("write_failed".to_owned()),
                    if retry {
                        "failed; retried within the hour"
                    } else {
                        "failed; needs an admin"
                    },
                )
            }
        };
        self.items.push(ReportItem {
            target,
            outcome,
            code,
            detail: Some(detail.to_owned()),
        });
    }

    fn report(self) -> Result<Report, JobError> {
        let skipped = self.waiting + self.skipped;
        let total = self.done + self.failed + skipped;
        let expectation = if total == 0 && self.handoffs_used == 0 && self.handoffs_dropped == 0 {
            Expectation::NotApplicable
        } else if self.failed > 0 {
            Expectation::Unmet(format!("{} of {total} account writes failed", self.failed))
        } else {
            Expectation::Met(format!(
                "{} written, {} wait for the gate",
                self.done, self.waiting
            ))
        };
        let mut report = Report::new(total, self.done, self.failed, skipped, expectation)?
            .with_counter("created", self.created)
            .with_counter("passwords", self.passwords)
            .with_counter("waiting", self.waiting)
            .with_counter("handoffs_used", self.handoffs_used)
            .with_counter("handoffs_dropped", self.handoffs_dropped);
        for item in self.items {
            report = report.with_item(item);
        }
        Ok(report)
    }
}

fn observed(account: &RemoteAccount) -> ObservedAccount {
    ObservedAccount {
        remote_id: account.remote_id.clone(),
        username: account.username.clone(),
        enabled: account.enabled,
        privileged: account.privileged,
        locked_by_service: account.locked_by_service,
        libraries: match &account.libraries {
            starr_core::accounts::LibraryGrant::All => None,
            starr_core::accounts::LibraryGrant::Only(ids) => Some(ids.clone()),
        },
    }
}

/// Per run: the gate and the adapter of each instance, decided once.
struct Instances<'a> {
    openers: Openers<'a>,
    gates: HashMap<i64, Gate>,
    adapters: HashMap<i64, Option<AnyAccounts>>,
}

impl Instances<'_> {
    async fn gate(&mut self, db: &Database, p: &Planned) -> Result<Gate, DbError> {
        if let Some(gate) = self.gates.get(&p.service_id) {
            return Ok(*gate);
        }
        let gate = gate_of(db, p, Utc::now()).await?;
        self.gates.insert(p.service_id, gate);
        Ok(gate)
    }

    async fn adapter(&mut self, db: &Database, id: i64) -> Result<Option<&AnyAccounts>, JobError> {
        if !self.adapters.contains_key(&id) {
            let adapter = match account_writes::open(db, self.openers, id).await? {
                Ok(adapter) => Some(adapter),
                Err(NotExecuted::Unavailable(failure)) => {
                    tracing::warn!(
                        service = id,
                        code = failure.code,
                        "instance unavailable for passwords"
                    );
                    None
                }
            };
            self.adapters.insert(id, adapter);
        }
        Ok(self.adapters.get(&id).and_then(Option::as_ref))
    }
}

/// The `accounts.password` handler.
pub(crate) async fn run(
    ctx: &JobContext,
    openers: Openers<'_>,
    key: &CredentialKey,
) -> Result<Report, JobError> {
    if ctx.cancel.is_cancelled() {
        return Err(JobError::Cancelled);
    }
    let db = &ctx.db;
    crate::provisioning::pause_for_totp_gap(
        db,
        (starr_db::audit::ActorKind::System, None),
        "accounts.password",
    )
    .await?;
    let now = Utc::now();
    let mut tally = Tally {
        handoffs_dropped: db.delete_expired_password_handoffs(now).await?,
        ..Tally::default()
    };
    let mut instances = Instances {
        openers,
        gates: HashMap::new(),
        adapters: HashMap::new(),
    };
    // A member may hand over a password while this run is under way; the
    // queued job is this one (deduplicated), so new handoffs are taken in
    // further rounds (bounded) instead of waiting for the next plan run.
    let mut seen: HashSet<(i64, i64)> = HashSet::new();
    for _ in 0..MAX_ROUNDS {
        let handoffs: Vec<StoredHandoff> = db
            .password_handoffs(Utc::now())
            .await?
            .into_iter()
            .filter(|h| seen.insert((h.member_id, h.generation)))
            .collect();
        if handoffs.is_empty() {
            break;
        }
        let planned = replan(db, Utc::now()).await?;
        instances.gates.clear();
        for handoff in &handoffs {
            if ctx.cancel.is_cancelled() {
                return Err(JobError::Cancelled);
            }
            let keep =
                member_handoff(ctx, key, &planned, &mut instances, handoff, &mut tally).await?;
            if !keep {
                db.delete_password_handoff(handoff.member_id, handoff.generation)
                    .await?;
                tally.handoffs_used += 1;
            }
        }
    }
    if tally.done > 0 || tally.failed > 0 {
        // Enable what got its password, plan what is left.
        enqueue_reconcile(db, Trigger::Manual).await?;
    }
    tally.report()
}

/// Write one member's handoff wherever it is due; `true` to keep the
/// handoff for a later run.
async fn member_handoff(
    ctx: &JobContext,
    key: &CredentialKey,
    planned: &[Planned],
    instances: &mut Instances<'_>,
    handoff: &StoredHandoff,
    tally: &mut Tally,
) -> Result<bool, JobError> {
    let db = &ctx.db;
    let Some(member) = db.get_member(handoff.member_id).await? else {
        return Ok(false);
    };
    let generation = db.member_password_generation(member.id).await?;
    if member.status != MemberStatus::Active || generation != Some(handoff.generation) {
        // Suspended or retired (D9), or a newer password replaces it.
        return Ok(false);
    }
    let Some(password) = key.open_handoff(
        member.id,
        handoff.generation,
        &handoff.recipient,
        &handoff.sealed,
    ) else {
        tracing::error!(
            member = member.id,
            "a password handoff does not open with this key for this member; dropped"
        );
        return Ok(false);
    };
    let with = WithPassword {
        password: &password,
        generation: handoff.generation,
    };
    let mut keep = false;
    for p in planned {
        let Some(entry) = p.plan.entries.iter().find(|e| e.member_id == member.id) else {
            continue;
        };
        if !entry.steps.iter().any(|s| s.action.needs_password()) {
            continue;
        }
        let target = Target {
            instance: p.service_id,
            service_name: &p.service_name,
            username: Some(&member.username),
        };
        let name = format!("{}:{}", p.service_name, member.username);
        let unit = if instances.gate(db, p).await? == Gate::Execute {
            match instances.adapter(db, p.service_id).await? {
                Some(adapter) => {
                    member_steps(db, adapter, &ctx.cancel, target, entry, &with, tally).await?
                }
                None => Unit::Failed { retry: true },
            }
        } else {
            Unit::Waiting
        };
        keep |= matches!(unit, Unit::Waiting | Unit::Failed { retry: true });
        tally.record(name, unit);
    }
    Ok(keep)
}

/// The member's steps on one instance: a create, or the steps on the owned
/// account with the password.
async fn member_steps(
    db: &Database,
    adapter: &AnyAccounts,
    cancel: &CancellationToken,
    at: Target<'_>,
    entry: &PlanEntry,
    with: &WithPassword<'_>,
    tally: &mut Tally,
) -> Result<Unit, JobError> {
    if let Some(step) = entry
        .steps
        .iter()
        .find(|s| matches!(s.action, Action::Create { .. }))
    {
        let unit = create(db, adapter, at, entry.member_id, step, with).await?;
        if unit == Unit::Done {
            tally.created += 1;
        }
        return Ok(unit);
    }
    let mut done = Executed::default();
    account_writes::execute_entry(db, adapter, cancel, at, entry, Some(with), &mut done).await?;
    Ok(if done.failed > 0 {
        Unit::Failed {
            retry: done.service_trouble,
        }
    } else if done.skipped > 0 {
        Unit::Skipped
    } else {
        tally.passwords += 1;
        Unit::Done
    })
}

/// Whether the member is still decided "create" there without an account
/// (an admin may have decided otherwise since the plan).
async fn still_to_create(db: &Database, instance: i64, member_id: i64) -> Result<bool, DbError> {
    Ok(db
        .member_service_accounts(member_id)
        .await?
        .iter()
        .any(|r| {
            r.service_instance_id == instance
                && r.decision == Decision::Create
                && r.remote_id.is_none()
        }))
}

/// Create the member's account with the password (D17 order inside the
/// adapter) and record the outcome.
async fn create(
    db: &Database,
    adapter: &AnyAccounts,
    at: Target<'_>,
    member_id: i64,
    step: &Step,
    with: &WithPassword<'_>,
) -> Result<Unit, JobError> {
    let Action::Create {
        username,
        libraries,
    } = &step.action
    else {
        return Ok(Unit::Skipped);
    };
    let instance = at.instance;
    let target = format!("{}:{}", at.service_name, username);
    let active = db
        .get_member(member_id)
        .await?
        .is_some_and(|m| m.status == MemberStatus::Active);
    if !active || !still_to_create(db, instance, member_id).await? {
        return Ok(Unit::Skipped);
    }
    let Some(libraries) = NonEmpty::new(libraries.clone()) else {
        return Ok(Unit::Skipped);
    };
    let now = Utc::now();
    let mut details = json!({
        "serviceId": instance,
        "reason": step.reason,
        "libraries": libraries.as_slice(),
    });
    let outcome = adapter
        .create_account(&NewAccount {
            username,
            password: with.password,
            libraries: &libraries,
        })
        .await;
    let (unit, failure): (Unit, Option<AdapterError>) = match outcome {
        Ok(CreateOutcome::Created(account)) => {
            let owned = db
                .record_created_account(
                    instance,
                    member_id,
                    &account.remote_id,
                    &account.username,
                    Some(with.generation),
                )
                .await?;
            details["remoteId"] = json!(account.remote_id);
            details["result"] = json!(if owned { "ok" } else { "unowned" });
            settle_created(db, adapter, instance, &target, account, owned).await?;
            (Unit::Done, None)
        }
        Ok(CreateOutcome::NameTaken { remote_id }) => {
            // The next scan shows it as a conflict for the admin.
            details["remoteId"] = json!(remote_id);
            details["result"] = json!("name_taken");
            let taken = AdapterError::Rejected("the name is taken there by another account".into());
            (Unit::Failed { retry: false }, Some(taken))
        }
        Ok(CreateOutcome::Unfinished { remote_id, error }) => {
            // Owned from now on, never enabled before a password reached
            // it (the planner disables it if it is still active).
            db.record_created_account(instance, member_id, &remote_id, username, None)
                .await?;
            // Without a flag the half-made account has a random password:
            // locked by Starr until the member's password reaches it (D9).
            if adapter.rules().disable == DisableMode::ScramblePassword {
                db.record_locked_by_starr(instance, member_id, &remote_id, now)
                    .await?;
            }
            // Into the inventory now, so the next plan keeps it disabled
            // (best effort; the next scan finds it anyway).
            if let Ok(account) = adapter.account(&remote_id).await {
                db.store_read_back(instance, &observed(&account), now)
                    .await?;
            }
            details["remoteId"] = json!(remote_id);
            details["result"] = json!("unfinished");
            let retry = service_trouble(&error);
            (Unit::Failed { retry }, Some(error))
        }
        Err(error) => {
            details["result"] = json!("failed");
            let retry = service_trouble(&error);
            (Unit::Failed { retry }, Some(error))
        }
    };
    let created = Created {
        instance,
        member_id,
        target: &target,
        step,
    };
    record_create(db, &created, failure.as_ref(), details).await?;
    Ok(unit)
}

/// Store a created account's read-back. One that is not the member's any
/// more (an admin decided otherwise while it was being created) is
/// disabled right away: Starr never leaves an unowned account usable with
/// the member's password; the admin sees it as a name-taken conflict.
async fn settle_created(
    db: &Database,
    adapter: &AnyAccounts,
    instance: i64,
    target: &str,
    account: RemoteAccount,
    owned: bool,
) -> Result<(), JobError> {
    let account = if owned {
        account
    } else {
        tracing::error!(
            target,
            "account created, but the decision changed meanwhile; disabled and left unowned"
        );
        match adapter.set_enabled(&account.remote_id, false).await {
            Ok(disabled) => disabled,
            Err(err) => {
                tracing::error!(
                    target,
                    code = err.code(),
                    "an unowned new account could not be disabled"
                );
                account
            }
        }
    };
    db.store_read_back(instance, &observed(&account), Utc::now())
        .await?;
    Ok(())
}

/// One create, for recording its outcome.
struct Created<'a> {
    instance: i64,
    member_id: i64,
    target: &'a str,
    step: &'a Step,
}

/// Record a create's outcome for "needs you" and audit it (no secret).
async fn record_create(
    db: &Database,
    c: &Created<'_>,
    failure: Option<&AdapterError>,
    mut details: serde_json::Value,
) -> Result<(), JobError> {
    let now = Utc::now();
    match failure {
        Some(err) => {
            let detail = err.to_string();
            tracing::warn!(target = c.target, code = err.code(), error = %detail, "account create failed");
            details["code"] = json!(err.code());
            db.record_account_write(
                c.instance,
                c.member_id,
                WriteResult::Failed {
                    code: err.code(),
                    detail: &detail,
                },
                now,
            )
            .await?;
        }
        None => {
            db.record_account_write(c.instance, c.member_id, WriteResult::Ok, now)
                .await?;
        }
    }
    audit(db, c.target, c.step, details).await;
    Ok(())
}
