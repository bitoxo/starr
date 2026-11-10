//! Account writes with safe mode (spec 0005 slice 4, M8, D15, M10 2),
//! admins only:
//! - `POST /api/v1/services/{id}/account-plan/confirm`: confirm the stored
//!   plan by its fingerprint: go live (plan only → live), resume paused
//!   writes, or approve a plan held by the destructive cap or the one-hour
//!   rule. Needs a fresh elevation;
//! - `POST /api/v1/services/{id}/account-writes/pause`: pause the writes of
//!   a live instance (no elevation: the safe direction);
//! - `GET /api/v1/accounts/attention`: held plans and failed writes for
//!   "needs you".
//!
//! Every instance starts plan only (also after the upgrade); nothing is
//! written until an admin confirms a complete plan at most an hour old.
//! Going live, resuming and approving are refused while an active admin
//! has no TOTP (spec 0005 M10 1, pulled forward into slice 5 by the
//! maintainer on 2026-11-03).

use axum::Json;
use axum::extract::{Path, State};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::provisioning::{
    Approval, Conflict, DESTRUCTIVE_CAP, Gate, GateInput, HoldReason, Plan, State as PlanState,
    WritesMode, gate,
};
use starr_db::account_writes::{AccountWrites, ConfirmOutcome, ConfirmRefusal, PauseOutcome};
use utoipa::ToSchema;

use super::common;
use super::error::{ApiError, ErrorBody};
use super::service_accounts::{AccountPlan, accounts_changed, admins_without_totp, service_plan};
use crate::AppState;
use crate::auth::AdminSession;

/// Whether account writes are on for an instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = AccountWritesMode)]
pub enum ModeDto {
    /// Never switched live: plans are computed and shown, nothing written.
    PlanOnly,
    /// Live, but paused by an admin: plan only.
    Paused,
    Live,
}

impl From<WritesMode> for ModeDto {
    fn from(m: WritesMode) -> Self {
        match m {
            WritesMode::PlanOnly => Self::PlanOnly,
            WritesMode::Paused => Self::Paused,
            WritesMode::Live => Self::Live,
        }
    }
}

/// What the next run does with the current plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = AccountWritesNext)]
pub enum NextDto {
    /// Account writes are off: plan only.
    PlanOnly,
    /// Writes paused: plan only.
    Paused,
    /// The plan rests on an incomplete listing: never executed.
    Incomplete,
    /// Waits for an admin's approval of exactly this plan.
    Held,
    /// Executed by the next run (an empty plan writes nothing).
    Execute,
}

/// Why a live plan waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = AccountHoldReason)]
pub enum HoldReasonDto {
    /// No complete live run in the last hour: an emergency change made in
    /// the service meanwhile is not silently undone.
    NoRecentRun,
    /// More destructive steps (disable, libraries taken away) than the cap.
    OverCap,
}

/// The account-write state of an instance and what the next run does.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = AccountWrites)]
pub struct AccountWritesDto {
    mode: ModeDto,
    next: NextDto,
    hold_reason: Option<HoldReasonDto>,
    /// Destructive steps executed within an hour without approval.
    cap: u32,
    /// Destructive steps executed on the instance in the last hour; they
    /// count against the cap together with the plan's own.
    recent_destructive: u32,
    live_since: Option<DateTime<Utc>>,
    live_by: Option<String>,
    paused_at: Option<DateTime<Utc>>,
    paused_by: Option<String>,
    last_live_run_at: Option<DateTime<Utc>>,
    /// An approval of the current plan is pending (the next run uses it).
    approved: bool,
    approved_at: Option<DateTime<Utc>>,
    approved_by: Option<String>,
}

/// The state shown for an instance with `writes`, whose stored plan is
/// `plan` (`None`: none yet) with `fingerprint`; `recent` destructive steps
/// were executed there within the last hour.
pub(crate) fn writes_dto(
    writes: &AccountWrites,
    plan: Option<(&Plan, &str)>,
    recent: u32,
    now: DateTime<Utc>,
) -> AccountWritesDto {
    let approval = writes.approval.as_ref().map(|a| Approval {
        fingerprint: &a.fingerprint,
        at: a.at,
    });
    let decided = plan.map(|(plan, fingerprint)| {
        gate(&GateInput {
            mode: writes.mode(),
            plan,
            fingerprint,
            last_live_run: writes.last_live_run_at,
            approval,
            recent_destructive: recent,
            now,
        })
    });
    let (next, hold_reason) = match decided {
        None => match writes.mode() {
            WritesMode::PlanOnly => (NextDto::PlanOnly, None),
            WritesMode::Paused => (NextDto::Paused, None),
            WritesMode::Live => (NextDto::Incomplete, None),
        },
        Some(Gate::PlanOnly) => (NextDto::PlanOnly, None),
        Some(Gate::Paused) => (NextDto::Paused, None),
        Some(Gate::Incomplete) => (NextDto::Incomplete, None),
        Some(Gate::Hold { reason }) => (
            NextDto::Held,
            Some(match reason {
                HoldReason::NoRecentRun => HoldReasonDto::NoRecentRun,
                HoldReason::OverCap { .. } => HoldReasonDto::OverCap,
            }),
        ),
        Some(Gate::Execute) => (NextDto::Execute, None),
    };
    let approved = plan.is_some_and(|(_, fingerprint)| {
        starr_core::provisioning::approves(approval, fingerprint, now)
    });
    AccountWritesDto {
        mode: writes.mode().into(),
        next,
        hold_reason,
        cap: DESTRUCTIVE_CAP,
        recent_destructive: recent,
        live_since: writes.live_since,
        live_by: writes.live_by.clone(),
        paused_at: writes.paused_at,
        paused_by: writes.paused_by.clone(),
        last_live_run_at: writes.last_live_run_at,
        approved,
        approved_at: writes.approval.as_ref().map(|a| a.at),
        approved_by: writes.approval.as_ref().and_then(|a| a.by.clone()),
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfirmPlanRequest {
    /// The fingerprint of the plan the admin saw (`AccountPlan.fingerprint`).
    fingerprint: String,
}

/// Confirm an instance's stored plan, exactly as shown (its fingerprint):
/// switch account writes live, resume paused writes, or approve a plan
/// that waits (more destructive steps than the cap, or no complete live
/// run in the last hour). The plan must be complete and at most an hour
/// old. Needs a fresh elevation (`POST /api/v1/auth/elevate`) and every
/// active admin to have TOTP on (M10 1: one phished admin password must not
/// be able to take over every member's media account; the plan lists who
/// lacks it as `adminsWithoutTotp`). Queues `accounts.reconcile`, which
/// executes exactly this plan.
#[utoipa::path(post, path = "/api/v1/services/{id}/account-plan/confirm", tag = "accounts",
    params(("id" = i64, Path, description = "Service id")),
    request_body = ConfirmPlanRequest,
    responses(
        (status = 200, body = AccountPlan),
        (status = 404, body = ErrorBody, description = "`services.not_found`"),
        (status = 409, body = ErrorBody, description = "`accounts.plan_changed` (the plan is not the one confirmed; look again), `accounts.totp_required` (an active admin has no TOTP; see `adminsWithoutTotp`)"),
        (status = 422, body = ErrorBody, description = "`accounts.unsupported`, `accounts.plan_incomplete`, `accounts.plan_stale` (older than an hour; scan again)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin), `auth.elevation_required`", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn confirm_plan(
    admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<ConfirmPlanRequest>,
) -> Result<Json<AccountPlan>, ApiError> {
    common::require_elevation(&state, &admin.0)?;
    // 404/422 for unknown instances and ones without accounts.
    let current = service_plan(&state, id).await?;
    let missing = admins_without_totp(&state).await?;
    if !missing.is_empty() {
        common::audit(
            &state,
            &admin.0.subject,
            "accounts.confirm_refused",
            Some(&current.service_name),
            Some(json!({ "serviceId": id, "reason": "totp_required", "admins": missing })),
        )
        .await?;
        return Err(ApiError::Conflict("accounts.totp_required"));
    }
    let now = state.clock.now();
    let outcome = state
        .db
        .confirm_account_plan(id, body.fingerprint.trim(), &admin.0.subject.username, now)
        .await?
        .ok_or(ApiError::NotFound("services.not_found"))?;
    let (went_live, resumed) = match outcome {
        ConfirmOutcome::Refused(ConfirmRefusal::PlanChanged) => {
            return Err(ApiError::Conflict("accounts.plan_changed"));
        }
        ConfirmOutcome::Refused(ConfirmRefusal::PlanIncomplete) => {
            return Err(ApiError::Validation("accounts.plan_incomplete"));
        }
        ConfirmOutcome::Refused(ConfirmRefusal::PlanStale) => {
            return Err(ApiError::Validation("accounts.plan_stale"));
        }
        ConfirmOutcome::Confirmed {
            went_live, resumed, ..
        } => (went_live, resumed),
    };
    let action = if went_live {
        "accounts.live"
    } else if resumed {
        "accounts.resume"
    } else {
        "accounts.approve"
    };
    common::audit(
        &state,
        &admin.0.subject,
        action,
        Some(&current.service_name),
        Some(json!({
            "serviceId": id,
            "fingerprint": body.fingerprint.trim(),
            "counts": current.plan.counts,
        })),
    )
    .await?;
    accounts_changed(&state).await?;
    Ok(Json(service_plan(&state, id).await?.plan))
}

/// Pause the account writes of a live instance: plans keep being computed
/// and shown, nothing is written until an admin resumes with the pending
/// plan in view. Only a change is audit-logged.
#[utoipa::path(post, path = "/api/v1/services/{id}/account-writes/pause", tag = "accounts",
    params(("id" = i64, Path, description = "Service id")),
    responses(
        (status = 200, body = AccountPlan),
        (status = 404, body = ErrorBody, description = "`services.not_found`"),
        (status = 422, body = ErrorBody, description = "`accounts.unsupported`, `accounts.not_live` (account writes were never switched on)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn pause_writes(
    admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<AccountPlan>, ApiError> {
    let current = service_plan(&state, id).await?;
    let outcome = state
        .db
        .pause_account_writes(id, &admin.0.subject.username, state.clock.now())
        .await?
        .ok_or(ApiError::NotFound("services.not_found"))?;
    match outcome {
        PauseOutcome::NotLive => return Err(ApiError::Validation("accounts.not_live")),
        PauseOutcome::Paused { changed: true, .. } => {
            common::audit(
                &state,
                &admin.0.subject,
                "accounts.pause",
                Some(&current.service_name),
                Some(json!({ "serviceId": id })),
            )
            .await?;
        }
        PauseOutcome::Paused { changed: false, .. } => {}
    }
    Ok(Json(service_plan(&state, id).await?.plan))
}

/// A plan that waits for an admin.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HeldPlan {
    pub(crate) service_id: i64,
    pub(crate) service_name: String,
    pub(crate) reason: HoldReasonDto,
    pub(crate) destructive: u32,
    pub(crate) cap: u32,
    /// Destructive steps already executed there in the last hour.
    pub(crate) recent: u32,
    pub(crate) computed_at: DateTime<Utc>,
}

/// What an account conflict is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum AccountConflictKind {
    /// Starr would create the account, but the name is taken there.
    NameTaken,
    /// The managed account has administrator rights in the service now;
    /// Starr keeps its hands off.
    Privileged,
    /// The managed account is no longer listed by the service.
    Missing,
    /// The service locked the account after failed logins; Starr never
    /// lifts that.
    LockedByService,
}

/// A managed account (or a planned one) that needs an admin.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountConflict {
    pub(crate) service_id: i64,
    pub(crate) service_name: String,
    pub(crate) member_id: i64,
    pub(crate) username: String,
    pub(crate) kind: AccountConflictKind,
    /// `missing`: last seen in a listing.
    pub(crate) since: Option<DateTime<Utc>>,
    /// `nameTaken`: the existing account with the wanted name, to link
    /// the member to instead (spec 0008 ADM-08).
    pub(crate) remote_id: Option<String>,
    /// `nameTaken`: its name in the service.
    pub(crate) remote_name: Option<String>,
}

/// A change made in the service since Starr's last write that the next
/// live run undoes (D13).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountDeviation {
    pub(crate) service_id: i64,
    pub(crate) service_name: String,
    pub(crate) member_id: i64,
    pub(crate) username: String,
    /// The steps that undo it.
    pub(crate) actions: Vec<super::service_accounts::ActionDto>,
}

/// A library a category maps that the service no longer lists (renamed or
/// removed there: a renamed Jellyfin library gets a new ID). Accounts
/// cannot be given it; map the library again.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StaleLibrary {
    pub(crate) service_id: i64,
    pub(crate) service_name: String,
    pub(crate) category: super::common::CategoryDto,
    /// The library's ID as mapped.
    pub(crate) library_id: String,
}

/// A failed account write.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FailedAccountWrite {
    pub(crate) service_id: i64,
    pub(crate) service_name: String,
    pub(crate) member_id: i64,
    pub(crate) username: String,
    /// Error taxonomy code (`transient`, `rejected`, …).
    pub(crate) code: String,
    pub(crate) detail: Option<String>,
    pub(crate) at: DateTime<Utc>,
}

/// An account Starr created that never got the member's password (a
/// create that did not finish, e.g. Emby refused the policy write right
/// after creating it). Starr keeps it disabled; the member's next password
/// entry ("activate") finishes it.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountWithoutPassword {
    pub(crate) service_id: i64,
    pub(crate) service_name: String,
    pub(crate) member_id: i64,
    pub(crate) username: String,
    pub(crate) remote_id: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountAttention {
    pub(crate) held: Vec<HeldPlan>,
    pub(crate) failures: Vec<FailedAccountWrite>,
    pub(crate) without_password: Vec<AccountWithoutPassword>,
    pub(crate) conflicts: Vec<AccountConflict>,
    pub(crate) deviations: Vec<AccountDeviation>,
    pub(crate) stale_libraries: Vec<StaleLibrary>,
}

/// What account provisioning needs from an admin ("needs you"): live plans
/// that wait for approval, writes that failed, accounts Starr created that
/// have no password yet, conflicts (name taken, admin rights gained, no
/// longer listed, locked by the service), deviations the next live run
/// undoes, and mapped libraries the service no longer lists.
#[utoipa::path(get, path = "/api/v1/accounts/attention", tag = "accounts",
    responses(
        (status = 200, body = AccountAttention),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn attention(
    _admin: AdminSession,
    State(state): State<AppState>,
) -> Result<Json<AccountAttention>, ApiError> {
    Ok(Json(account_attention(&state).await?))
}

/// What account provisioning needs from an admin (also the Inbox's account
/// items, spec 0008 F9).
pub(crate) async fn account_attention(state: &AppState) -> Result<AccountAttention, ApiError> {
    let now = state.clock.now();
    let usernames: std::collections::HashMap<i64, String> =
        starr_worker::provisioning::plan_members(&state.db, now)
            .await?
            .into_iter()
            .map(|m| (m.id, m.username))
            .collect();
    let mut held = Vec::new();
    let mut conflicts = Vec::new();
    let mut deviations = Vec::new();
    let mut stale_libraries = Vec::new();
    for (id, name, roles) in starr_worker::provisioning::account_instances(&state.db).await? {
        stale_libraries.extend(stale_mappings(state, id, &name, &roles).await?);
        let Some(writes) = state.db.account_writes(id).await? else {
            continue;
        };
        let Some(stored) = state.db.account_plan(id).await? else {
            continue;
        };
        let Ok(plan) = serde_json::from_str::<Plan>(&stored.plan) else {
            continue;
        };
        let accounts = state.db.remote_accounts(id).await?;
        for entry in &plan.entries {
            let Some(username) = usernames.get(&entry.member_id) else {
                continue;
            };
            let at = (id, name.as_str(), username.as_str());
            conflicts.extend(conflict_of(at, entry, &accounts));
            deviations.extend(deviation_of(at, entry));
        }
        let recent = state.db.recent_destructive_writes(id, now).await?;
        let dto = writes_dto(&writes, Some((&plan, &stored.fingerprint)), recent, now);
        if let (NextDto::Held, Some(reason)) = (dto.next, dto.hold_reason) {
            held.push(HeldPlan {
                service_id: id,
                service_name: name,
                reason,
                destructive: plan.executable_steps().1,
                cap: DESTRUCTIVE_CAP,
                recent,
                computed_at: stored.computed_at,
            });
        }
    }
    let failures = state
        .db
        .account_write_failures()
        .await?
        .into_iter()
        .map(|f| FailedAccountWrite {
            service_id: f.service_id,
            service_name: f.service_name,
            member_id: f.member_id,
            username: f.username,
            code: f.code,
            detail: f.detail,
            at: f.at,
        })
        .collect();
    let without_password = state
        .db
        .accounts_without_password()
        .await?
        .into_iter()
        .map(|a| AccountWithoutPassword {
            service_id: a.service_id,
            service_name: a.service_name,
            member_id: a.member_id,
            username: a.username,
            remote_id: a.remote_id,
        })
        .collect();
    Ok(AccountAttention {
        held,
        failures,
        without_password,
        conflicts,
        deviations,
        stale_libraries,
    })
}

/// The conflict of one plan entry on instance (id, name) for `username`.
fn conflict_of(
    (id, name, username): (i64, &str, &str),
    entry: &starr_core::provisioning::PlanEntry,
    accounts: &[starr_db::remote_accounts::RemoteAccountRow],
) -> Option<AccountConflict> {
    let taken = match &entry.state {
        PlanState::Conflict {
            conflict: Conflict::NameTaken { remote_id },
        } => accounts.iter().find(|a| &a.remote_id == remote_id),
        _ => None,
    };
    let kind = match &entry.state {
        PlanState::Conflict {
            conflict: Conflict::NameTaken { .. },
        } => AccountConflictKind::NameTaken,
        PlanState::Conflict {
            conflict: Conflict::Privileged,
        } => AccountConflictKind::Privileged,
        PlanState::Conflict {
            conflict: Conflict::Missing,
        } => AccountConflictKind::Missing,
        PlanState::LockedByService => AccountConflictKind::LockedByService,
        _ => return None,
    };
    let since = (kind == AccountConflictKind::Missing)
        .then(|| {
            let remote = entry.remote_id.as_deref()?;
            accounts
                .iter()
                .find(|a| a.remote_id == remote)
                .map(|a| a.last_seen)
        })
        .flatten();
    Some(AccountConflict {
        service_id: id,
        service_name: name.to_owned(),
        member_id: entry.member_id,
        username: username.to_owned(),
        kind,
        since,
        remote_id: taken.map(|a| a.remote_id.clone()),
        remote_name: taken.map(|a| a.username.clone()),
    })
}

/// The steps of one plan entry that undo a change made in the service.
fn deviation_of(
    (id, name, username): (i64, &str, &str),
    entry: &starr_core::provisioning::PlanEntry,
) -> Option<AccountDeviation> {
    let undo: Vec<_> = entry
        .steps
        .iter()
        .filter(|s| s.drift)
        .map(|s| super::service_accounts::action_dto(&s.action))
        .collect();
    (!undo.is_empty()).then(|| AccountDeviation {
        service_id: id,
        service_name: name.to_owned(),
        member_id: entry.member_id,
        username: username.to_owned(),
        actions: undo,
    })
}

/// Libraries the consume `roles` of instance `id` map that its last
/// successful library listing no longer contains.
async fn stale_mappings(
    state: &AppState,
    id: i64,
    name: &str,
    roles: &[(
        starr_core::members::Category,
        starr_core::libraries::ConsumeSettings,
    )],
) -> Result<Vec<StaleLibrary>, ApiError> {
    let listing = state.db.library_listing_state(id).await?;
    // Without a successful listing nothing is known to be gone.
    if listing.is_none_or(|l| l.listed_at.is_none() || l.error.is_some()) {
        return Ok(vec![]);
    }
    let listed: std::collections::HashSet<String> = state
        .db
        .service_libraries(id)
        .await?
        .into_iter()
        .map(|l| l.library.id)
        .collect();
    let mut out = Vec::new();
    for (category, settings) in roles {
        for mapped in &settings.libraries {
            if !listed.contains(&mapped.id) {
                out.push(StaleLibrary {
                    service_id: id,
                    service_name: name.to_owned(),
                    category: (*category).into(),
                    library_id: mapped.id.clone(),
                });
            }
        }
    }
    Ok(out)
}
