//! `accounts.reconcile` (spec 0005 slices 3 and 4): gather the desired
//! state (members, entitlements, the libraries each category maps) and what
//! the last `accounts.scan` observed, run the pure planner
//! (`starr_core::provisioning::plan`) per consume instance with accounts
//! and store the plan for the admin pages. Then the safe-mode gate
//! (`starr_core::provisioning::gate`, M8, D15) decides per instance: plan
//! only (never switched live, or paused), never on an incomplete plan,
//! held for an admin's approval (over the destructive cap, or no complete
//! live run in the last hour), or executed ([`crate::account_writes`]).
//! Planning reads the database only; only an executed plan opens an
//! adapter, and it writes nothing but enable/disable and library lists.
//! Creates and passwords need the member's password and run in
//! `accounts.password` (ADR 0010); a live run queues it while handoffs
//! wait, and tells members whose account waits for their password
//! ("activate" notification).
//!
//! Slice 6: members whose deletion order is due (also tombstones that
//! still own accounts) are planned with their accounts deleted (D16);
//! accounts of theirs that are gone from the service already are released.
//! After a complete live run the accounts found in sync become the drift
//! baseline (D13), and admins are told once about managed accounts that
//! gained admin rights or were locked by the service (D2, D10).

use std::collections::HashMap;
use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use starr_core::accounts::DisableMode;
use starr_core::categories::RoleKind;
use starr_core::jobs::{Expectation, ItemOutcome, JobKind, Report, ReportItem, Trigger};
use starr_core::libraries::{self, ConsumeSettings, RoleLibraries};
use starr_core::members::{Category, Entitlement, MemberStatus};
use starr_core::notifications::NotificationKind;
use starr_core::provisioning::{
    self, Approval, Conflict, Counts, Gate, GateInput, HoldReason, Instance, Link, Observed,
    PasswordNeed, Plan, State, Wanted, WritesMode, gate,
};
use starr_db::audit::{ActorKind, AuditEntry};
use starr_db::jobs::NewJob;
use starr_db::members::MemberFilter;
use starr_db::service_accounts::StoredPlan;
use starr_db::{Database, DbError};

use crate::account_writes::{self, Executed, NotExecuted};
use crate::services::{Failure, Openers};
use crate::{JobContext, JobError, adapters};

/// The plan of one instance as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub service_id: i64,
    pub service_name: String,
    pub plan: Plan,
    pub counts: Counts,
    pub fingerprint: String,
}

/// The consume instances whose adapter has accounts, with their consume
/// roles (category and mapped libraries).
pub async fn account_instances(
    db: &Database,
) -> Result<Vec<(i64, String, Vec<(Category, ConsumeSettings)>)>, DbError> {
    let mut by_instance: HashMap<i64, (String, Vec<(Category, ConsumeSettings)>)> = HashMap::new();
    for role in db.list_roles(None).await? {
        if role.kind != RoleKind::Consume || !adapters::has_accounts(&role.adapter_type) {
            continue;
        }
        let settings = role.consume_settings()?;
        by_instance
            .entry(role.service_instance_id)
            .or_insert_with(|| (role.service_name.clone(), Vec::new()))
            .1
            .push((role.category, settings));
    }
    let mut out: Vec<_> = by_instance
        .into_iter()
        .map(|(id, (name, roles))| (id, name, roles))
        .collect();
    out.sort_by_key(|(id, _, _)| *id);
    Ok(out)
}

/// A stable fingerprint of what a plan would do: whether it is complete
/// and every member's steps with the account they apply to (no time, no
/// states without steps, so a new suggestion for an undecided member does
/// not invalidate an approval). An approval names it.
fn fingerprint(plan: &Plan) -> Result<String, DbError> {
    let steps: Vec<_> = plan
        .entries
        .iter()
        .filter(|e| !e.steps.is_empty())
        .map(|e| (e.member_id, e.remote_id.as_deref(), &e.steps))
        .collect();
    let json = serde_json::to_string(&(plan.complete, steps))?;
    Ok(Sha256::digest(json.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        }))
}

/// A member as the planner sees them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanMember {
    pub id: i64,
    pub username: String,
    pub status: MemberStatus,
    /// The deletion order is due (D16).
    pub deletion_due: bool,
}

/// Every member to plan: all that are not tombstones, and tombstones whose
/// deletion order still has accounts to delete (D16).
pub async fn plan_members(db: &Database, now: DateTime<Utc>) -> Result<Vec<PlanMember>, DbError> {
    let due = db.due_deletions(now).await?;
    let mut out: Vec<PlanMember> = db
        .list_members(MemberFilter::default())
        .await?
        .into_iter()
        .map(|m| PlanMember {
            deletion_due: due.iter().any(|d| d.member_id == m.id),
            id: m.id,
            username: m.username,
            status: m.status,
        })
        .collect();
    for d in due.into_iter().filter(|d| d.tombstoned) {
        out.push(PlanMember {
            id: d.member_id,
            username: d.username,
            status: MemberStatus::Retired,
            deletion_due: true,
        });
    }
    out.sort_by_key(|m| m.id);
    Ok(out)
}

/// Release the accounts of due deletions that are gone from the service
/// already (nothing left to delete), audited as `account.unlink`.
async fn release_gone(db: &Database, now: DateTime<Utc>) -> Result<(), DbError> {
    for released in db.release_gone_accounts_of_due_deletions(now).await? {
        let target = format!("{}:{}", released.service_name, released.username);
        let details = json!({
            "serviceId": released.service_instance_id,
            "remoteId": released.remote_id,
            "reason": released.reason.as_str(),
        })
        .to_string();
        let entry = AuditEntry {
            actor_kind: ActorKind::System,
            actor_name: None,
            action: "account.unlink",
            target: Some(&target),
            details: Some(&details),
        };
        if let Err(err) = db.audit(&entry, now).await {
            tracing::error!(target, error = %err, "account released but audit entry not written");
        }
    }
    Ok(())
}

/// What every member should have on one instance (consume `roles`), with
/// Starr's link there.
fn wanted(
    members: &[PlanMember],
    entitlements: &HashMap<i64, Vec<Entitlement>>,
    enabled: &[Category],
    roles: &[(Category, ConsumeSettings)],
    links: &HashMap<i64, Link>,
) -> Vec<(Wanted, Option<Link>)> {
    let role_libraries: Vec<RoleLibraries<'_>> = roles
        .iter()
        .map(|(category, settings)| RoleLibraries {
            category: *category,
            libraries: &settings.libraries,
        })
        .collect();
    members
        .iter()
        .map(|m| {
            let ents = entitlements
                .get(&m.id)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let unmapped = roles
                .iter()
                .filter(|(c, s)| s.libraries.is_empty() && libraries::entitled(ents, enabled, *c))
                .map(|(c, _)| *c)
                .collect();
            let wanted = Wanted {
                member_id: m.id,
                username: m.username.clone(),
                status: m.status,
                libraries: libraries::desired(ents, enabled, &role_libraries)
                    .into_iter()
                    .map(|l| l.grant_id)
                    .collect(),
                unmapped,
                deletion_due: m.deletion_due,
            };
            (wanted, links.get(&m.id).cloned())
        })
        .collect()
}

/// Starr's link per member on one instance, from its stored rows and the
/// members' password generations.
fn links(
    rows: Vec<starr_db::service_accounts::ServiceAccountRow>,
    generations: &HashMap<i64, i64>,
) -> HashMap<i64, Link> {
    rows.into_iter()
        .map(|row| {
            let generation = generations.get(&row.member_id).copied().unwrap_or(0);
            let password = match row.remote_id {
                Some(_) => PasswordNeed::of(row.password, row.password_written, generation),
                None => PasswordNeed::Nothing,
            };
            (
                row.member_id,
                Link {
                    decision: row.decision,
                    remote_id: row.remote_id,
                    password,
                    expected: row.expected,
                    origin: row.origin,
                },
            )
        })
        .collect()
}

/// Plan every consume instance with accounts and store the plans; plans
/// of instances that no longer have accounts are dropped.
pub async fn replan(db: &Database, now: DateTime<Utc>) -> Result<Vec<Planned>, DbError> {
    let instances = account_instances(db).await?;
    let enabled: Vec<Category> = db
        .list_categories()
        .await?
        .into_iter()
        .filter(|c| c.enabled)
        .map(|c| c.category)
        .collect();
    release_gone(db, now).await?;
    let members = plan_members(db, now).await?;
    let mut entitlements = HashMap::with_capacity(members.len());
    let mut generations = HashMap::with_capacity(members.len());
    for m in &members {
        entitlements.insert(m.id, db.entitlements(m.id).await?);
        let generation = db.member_password_generation(m.id).await?.unwrap_or(0);
        generations.insert(m.id, generation);
    }

    let mut out = Vec::with_capacity(instances.len());
    for (id, name, roles) in &instances {
        let scan = db.account_scan_state(*id).await?;
        let listing_complete = scan.is_some_and(|s| s.complete == Some(true) && s.error.is_none());
        let observed: Vec<Observed> = db
            .remote_accounts(*id)
            .await?
            .into_iter()
            .map(|r| Observed {
                gone: r.gone_since.is_some(),
                ignored: r.ignored_at.is_some(),
                remote_id: r.remote_id,
                username: r.username,
                enabled: r.enabled,
                privileged: r.privileged,
                locked_by_service: r.locked_by_service,
                libraries: r.libraries,
                locked_by_starr: r.locked_by_starr,
            })
            .collect();
        let links = links(db.service_accounts(*id).await?, &generations);
        // How the service disables (Navidrome: no flag, D9).
        let disable = db
            .get_service(*id)
            .await?
            .and_then(|s| adapters::account_rules(&s.adapter_type))
            .map_or(DisableMode::Flag, |r| r.disable);
        let wanted = wanted(&members, &entitlements, &enabled, roles, &links);
        let plan = provisioning::plan(
            &Instance {
                listing_complete,
                disable,
                accounts: &observed,
            },
            &wanted,
        );
        let counts = plan.counts();
        // A failure with nothing left to retry is settled (needs you); a
        // step waiting for the member's password is still to retry.
        let pending: Vec<i64> = plan
            .entries
            .iter()
            .filter(|e| !e.steps.is_empty())
            .map(|e| e.member_id)
            .collect();
        db.clear_settled_write_failures(*id, &pending).await?;
        let plan_json = serde_json::to_string(&plan)?;
        let fingerprint = fingerprint(&plan)?;
        let stored = db
            .store_account_plan(&StoredPlan {
                service_instance_id: *id,
                computed_at: now,
                complete: plan.complete,
                fingerprint: fingerprint.clone(),
                plan: plan_json,
                counts: serde_json::to_string(&counts)?,
            })
            .await?;
        if stored {
            out.push(Planned {
                service_id: *id,
                service_name: name.clone(),
                plan,
                counts,
                fingerprint,
            });
        }
    }
    let keep: Vec<i64> = instances.iter().map(|(id, _, _)| *id).collect();
    db.prune_account_plans(&keep).await?;
    Ok(out)
}

/// How one instance's plan went in this run.
enum Outcome {
    /// Plan only, paused, incomplete or held: nothing written.
    NotRun(Gate),
    Executed(Executed),
    /// Live, but the instance could not be opened.
    Unavailable(Failure),
}

/// The safe-mode decision for one stored plan.
pub(crate) async fn gate_of(
    db: &Database,
    p: &Planned,
    now: DateTime<Utc>,
) -> Result<Gate, DbError> {
    let Some(writes) = db.account_writes(p.service_id).await? else {
        return Ok(Gate::PlanOnly);
    };
    let recent_destructive = db.recent_destructive_writes(p.service_id, now).await?;
    Ok(gate(&GateInput {
        mode: writes.mode(),
        plan: &p.plan,
        fingerprint: &p.fingerprint,
        last_live_run: writes.last_live_run_at,
        approval: writes.approval.as_ref().map(|a| Approval {
            fingerprint: &a.fingerprint,
            at: a.at,
        }),
        recent_destructive,
        now,
    }))
}

fn gate_detail(gate: Gate) -> &'static str {
    match gate {
        Gate::PlanOnly => "plan only (account writes are off)",
        Gate::Paused => "plan only (writes paused)",
        Gate::Incomplete => "plan only (listing incomplete; never executed)",
        Gate::Hold {
            reason: HoldReason::NoRecentRun,
        } => "waits for approval (no complete live run in the last hour)",
        Gate::Hold {
            reason: HoldReason::OverCap { .. },
        } => "waits for approval (more destructive steps than the cap within an hour)",
        Gate::Execute => "executed",
    }
}

/// Totals over the instances of one run.
#[derive(Default)]
struct Tally {
    sum: Counts,
    ran: Executed,
    incomplete: u64,
    held: u64,
    failed_instances: u64,
}

impl Tally {
    /// Count one instance's plan and outcome; its report item.
    fn record(&mut self, p: &Planned, outcome: Outcome) -> ReportItem {
        let c = p.counts;
        self.sum.create += c.create;
        self.sum.enable += c.enable;
        self.sum.disable += c.disable;
        self.sum.libraries += c.libraries;
        self.sum.delete += c.delete;
        self.sum.destructive += c.destructive;
        self.sum.drift += c.drift;
        self.sum.undecided += c.undecided;
        self.sum.conflicts += c.conflicts;
        self.sum.waiting += c.waiting;
        if !p.plan.complete {
            self.incomplete += 1;
        }
        let counts = format!(
            "create {}, enable {}, disable {}, libraries {}, delete {}; deviations {}; undecided {}, conflicts {}, waiting {}",
            c.create,
            c.enable,
            c.disable,
            c.libraries,
            c.delete,
            c.drift,
            c.undecided,
            c.conflicts,
            c.waiting
        );
        let target = p.service_name.clone();
        match outcome {
            Outcome::NotRun(gate) => {
                if matches!(gate, Gate::Hold { .. }) {
                    self.held += 1;
                }
                ReportItem {
                    target,
                    outcome: ItemOutcome::Succeeded,
                    code: None,
                    detail: Some(format!("{}: {counts}", gate_detail(gate))),
                }
            }
            Outcome::Executed(done) => {
                self.ran.applied += done.applied;
                self.ran.failed += done.failed;
                self.ran.skipped += done.skipped;
                let failed = done.failed > 0;
                if failed {
                    self.failed_instances += 1;
                }
                ReportItem {
                    target,
                    outcome: if failed {
                        ItemOutcome::Failed
                    } else {
                        ItemOutcome::Succeeded
                    },
                    code: failed.then(|| "write_failed".to_owned()),
                    detail: Some(format!(
                        "executed: applied {}, failed {}, skipped {}; {counts}",
                        done.applied, done.failed, done.skipped
                    )),
                }
            }
            Outcome::Unavailable(failure) => {
                self.failed_instances += 1;
                ReportItem {
                    target,
                    outcome: ItemOutcome::Failed,
                    code: Some(failure.code.to_owned()),
                    detail: Some(failure.detail),
                }
            }
        }
    }

    fn report(self, total: u64, items: Vec<ReportItem>) -> Result<Report, JobError> {
        let expectation = if total == 0 {
            Expectation::NotApplicable
        } else if self.failed_instances > 0 {
            Expectation::Unmet(format!(
                "{} account writes failed on {} of {total} instances",
                self.ran.failed, self.failed_instances
            ))
        } else if self.incomplete > 0 {
            Expectation::Unmet(format!(
                "{} of {total} plans rest on an incomplete listing",
                self.incomplete
            ))
        } else {
            Expectation::Met(format!(
                "{total} instances planned; {} steps applied, {} plans wait for approval",
                self.ran.applied, self.held
            ))
        };
        let ok = total - self.failed_instances;
        let mut report = Report::new(total, ok, self.failed_instances, 0, expectation)?;
        let (sum, ran) = (self.sum, self.ran);
        for (name, value) in [
            ("create", sum.create),
            ("enable", sum.enable),
            ("disable", sum.disable),
            ("libraries", sum.libraries),
            ("delete", sum.delete),
            ("destructive", sum.destructive),
            ("drift", sum.drift),
            ("undecided", sum.undecided),
            ("conflicts", sum.conflicts),
            ("waiting", sum.waiting),
            ("applied", ran.applied),
            ("failed", ran.failed),
            ("skipped", ran.skipped),
        ] {
            report = report.with_counter(name, u64::from(value));
        }
        report = report
            .with_counter("incomplete", self.incomplete)
            .with_counter("held", self.held);
        for item in items {
            report = report.with_item(item);
        }
        Ok(report)
    }
}

/// Run one instance's plan through the gate and execute it if allowed.
async fn run_instance(
    ctx: &JobContext,
    openers: Openers<'_>,
    p: &Planned,
    usernames: &HashMap<i64, String>,
    now: DateTime<Utc>,
) -> Result<Outcome, JobError> {
    let gate = gate_of(&ctx.db, p, now).await?;
    if gate != Gate::Execute {
        return Ok(Outcome::NotRun(gate));
    }
    let result = account_writes::execute(
        &ctx.db,
        openers,
        &ctx.cancel,
        p.service_id,
        &p.service_name,
        &p.plan,
        |id| usernames.get(&id).cloned(),
    )
    .await?;
    Ok(match result {
        Ok(done) => {
            // Trouble with the service itself (down, throttled, key
            // refused) or a pause during the run is no complete live run
            // (M8 A); the approval stays for the rest.
            if !done.service_trouble && !done.interrupted {
                ctx.db
                    .record_live_run(p.service_id, &p.fingerprint, Utc::now())
                    .await?;
                // Accounts with nothing to do are in sync: the baseline
                // for drift from now on (D13).
                let in_sync: Vec<i64> = p
                    .plan
                    .entries
                    .iter()
                    .filter(|e| e.steps.is_empty() && e.state == State::Managed)
                    .map(|e| e.member_id)
                    .collect();
                ctx.db.record_in_sync(p.service_id, &in_sync).await?;
            }
            Outcome::Executed(done)
        }
        Err(NotExecuted::Unavailable(failure)) => Outcome::Unavailable(failure),
    })
}

/// The `accounts.reconcile` handler: plan every instance, execute where
/// the gate lets it.
pub(crate) async fn reconcile(ctx: &JobContext, openers: Openers<'_>) -> Result<Report, JobError> {
    if ctx.cancel.is_cancelled() {
        return Err(JobError::Cancelled);
    }
    pause_for_totp_gap(&ctx.db, (ActorKind::System, None), "accounts.reconcile").await?;
    let now = Utc::now();
    let planned = replan(&ctx.db, now).await?;
    let usernames: HashMap<i64, String> = plan_members(&ctx.db, now)
        .await?
        .into_iter()
        .map(|m| (m.id, m.username))
        .collect();
    let mut tally = Tally::default();
    let mut items = Vec::with_capacity(planned.len());
    let mut executed = false;
    for p in &planned {
        let outcome = run_instance(ctx, openers, p, &usernames, now).await?;
        executed |= matches!(outcome, Outcome::Executed(_));
        items.push(tally.record(p, outcome));
    }
    // Plan again from the read-backs, so the pages show what is left.
    let planned = if executed {
        replan(&ctx.db, Utc::now()).await?
    } else {
        planned
    };
    let notified = after_live_run(&ctx.db, &planned, executed).await?;
    let admins_notified = notify_attention(&ctx.db, &planned, &usernames).await?;
    Ok(tally
        .report(planned.len() as u64, items)?
        .with_counter("activate_notified", notified)
        .with_counter("admins_notified", admins_notified))
}

/// Tell the admins once about managed accounts that gained administrator
/// rights in the service (D2, D13) or were locked by it (D10).
async fn notify_attention(
    db: &Database,
    planned: &[Planned],
    usernames: &HashMap<i64, String>,
) -> Result<u64, DbError> {
    let now = Utc::now();
    let mut notified = 0;
    for p in planned {
        for entry in &p.plan.entries {
            let kind = match entry.state {
                State::Conflict {
                    conflict: Conflict::Privileged,
                } => NotificationKind::AccountPrivileged,
                State::LockedByService => NotificationKind::AccountLocked,
                _ => continue,
            };
            let (Some(remote_id), Some(username)) =
                (entry.remote_id.as_deref(), usernames.get(&entry.member_id))
            else {
                continue;
            };
            if db
                .notify_account_attention(
                    kind,
                    p.service_id,
                    &p.service_name,
                    remote_id,
                    username,
                    now,
                )
                .await?
            {
                notified += 1;
            }
        }
    }
    Ok(notified)
}

/// Members whose account waits for their password: with a handoff in
/// flight, `accounts.password` takes it (queued when a live plan ran);
/// without one, on an instance whose writes are live, the member is told
/// once per instance and password generation ("activate", ADR 0010).
async fn after_live_run(
    db: &Database,
    planned: &[Planned],
    executed: bool,
) -> Result<u64, DbError> {
    let now = Utc::now();
    let handoffs = db.members_with_password_handoff(now).await?;
    if executed && !handoffs.is_empty() {
        enqueue_password(db).await?;
    }
    let mut notified = 0;
    for p in planned {
        let live = db
            .account_writes(p.service_id)
            .await?
            .is_some_and(|w| w.mode() == WritesMode::Live);
        if !live || !p.plan.complete {
            continue;
        }
        for entry in &p.plan.entries {
            // A switch whose handoff expired is the member's choice to
            // retry ("uses your old password"), not an "activate".
            let waits = entry.steps.iter().any(|s| {
                s.action.needs_password()
                    && s.reason != starr_core::provisioning::Reason::PasswordSwitch
            });
            if !waits || handoffs.contains(&entry.member_id) {
                continue;
            }
            let generation = db
                .member_password_generation(entry.member_id)
                .await?
                .unwrap_or(0);
            if db
                .notify_account_activate(
                    entry.member_id,
                    p.service_id,
                    &p.service_name,
                    generation,
                    now,
                )
                .await?
            {
                notified += 1;
            }
        }
    }
    Ok(notified)
}

/// Who an automatic pause is recorded as (`writes_paused_by`).
pub const AUTO_PAUSE_BY: &str = "starr";

/// The usernames of active admins without TOTP (spec 0005 M10 1).
pub async fn admins_without_totp(db: &Database) -> Result<Vec<String>, DbError> {
    Ok(db
        .list_admins()
        .await?
        .into_iter()
        .filter(|a| !a.suspended && !a.removed && !a.totp_enabled)
        .map(|a| a.username)
        .collect())
}

/// M10 1 (maintainer decision 2026-11-03): disabling TOTP stays allowed,
/// but while an active admin has no TOTP, account writes pause on every
/// live instance (resuming then needs TOTP again: the confirm gate). Called
/// where an admin can lose TOTP or become active without it (TOTP off,
/// admin created or reactivated, the recovery CLI), and before every
/// account-writing job as a backstop. Each instance paused now is audited
/// as `accounts.pause` with the reason, attributed to `actor`.
pub async fn pause_for_totp_gap(
    db: &Database,
    actor: (ActorKind, Option<&str>),
    trigger: &str,
) -> Result<Vec<(i64, String)>, DbError> {
    let missing = admins_without_totp(db).await?;
    if missing.is_empty() {
        return Ok(vec![]);
    }
    let now = Utc::now();
    let paused = db.pause_all_live_account_writes(AUTO_PAUSE_BY, now).await?;
    for (id, name) in &paused {
        let details = json!({
            "serviceId": id,
            "automatic": true,
            "reason": "adminWithoutTotp",
            "admins": missing,
            "trigger": trigger,
        })
        .to_string();
        let entry = AuditEntry {
            actor_kind: actor.0,
            actor_name: actor.1,
            action: "accounts.pause",
            target: Some(name),
            details: Some(&details),
        };
        if let Err(err) = db.audit(&entry, now).await {
            tracing::error!(service = id, error = %err, "writes paused but audit entry not written");
        }
        tracing::warn!(
            service = id,
            "account writes paused: an active admin has no TOTP"
        );
    }
    Ok(paused)
}

/// Queue `accounts.password` (deduplicated with a pending one): every
/// password handoff in flight is written by the next run.
pub async fn enqueue_password(db: &Database) -> Result<(), DbError> {
    let now = Utc::now();
    let kind = JobKind::AccountsPassword;
    let key = kind.unique_key();
    db.enqueue_job(
        &NewJob {
            kind: kind.name(),
            payload: "{}",
            unique_key: Some(&key),
            trigger: Trigger::Manual,
            run_after: now,
            max_attempts: kind.spec().max_attempts,
        },
        now,
    )
    .await?;
    Ok(())
}

/// Queue `accounts.reconcile` (deduplicated with a pending one).
pub async fn enqueue_reconcile(db: &Database, trigger: Trigger) -> Result<(), DbError> {
    let now = Utc::now();
    let kind = JobKind::AccountsReconcile;
    let key = kind.unique_key();
    db.enqueue_job(
        &NewJob {
            kind: kind.name(),
            payload: "{}",
            unique_key: Some(&key),
            trigger,
            run_after: now,
            max_attempts: kind.spec().max_attempts,
        },
        now,
    )
    .await?;
    Ok(())
}
