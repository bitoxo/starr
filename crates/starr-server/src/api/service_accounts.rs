//! Service accounts, adoption and the account plan (spec 0005 slice 3),
//! admins only:
//! - `/api/v1/accounts/matrix`: members × consume instances with accounts,
//!   each cell with the admin's decision, the owned account, a suggestion
//!   (equal name, never a link) and the planned state;
//! - `PUT /api/v1/members/{id}/service-accounts/{serviceId}`: decide
//!   (undecided / create / link an existing account / none);
//! - `POST /api/v1/members/{id}/service-accounts/confirm-suggestions`:
//!   link every suggested account of a member's row at once (M3);
//! - `/api/v1/services/{id}/account-plan` and
//!   `/api/v1/members/{id}/account-plan`: the stored plan.
//!
//! Nothing here writes to a service: `accounts.reconcile` executes plans
//! only on instances an admin switched live (`account_writes`). A decision
//! replans at once, so the admin sees its effect; other changes
//! (entitlements, member status, mappings) queue `accounts.reconcile`.
//! Linking an existing account (adopting) needs a fresh elevation (M10 2).

use std::collections::{HashMap, HashSet};

use axum::Json;
use axum::extract::{Path, State};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::accounts::DisableMode;
use starr_core::jobs::Trigger;
use starr_core::libraries;
use starr_core::members::{Category, Entitlement};
use starr_core::provisioning::{
    self, Action, Conflict, Counts, Decision, HandsOffReason, Observed, Origin, PasswordUse, Plan,
    PlanEntry, Reason, State as PlanState, Step, WaitReason,
};
use starr_db::audit::{ActorKind, AuditEntry};
use starr_db::members::{MemberFilter, MemberRecord};
use starr_db::remote_accounts::RemoteAccountRow;
use starr_db::service_accounts::{
    DecideOutcome, DecisionChange, LinkRefusal, PresetAccount, ServiceAccountRow,
};
use starr_worker::adapters;
use starr_worker::provisioning::{account_instances, enqueue_reconcile, plan_members, replan};
use utoipa::ToSchema;

use super::account_writes::{AccountWritesDto, writes_dto};
use super::common;
use super::error::{ApiError, ErrorBody};
use super::members::MemberStatusDto;
use crate::AppState;
use crate::auth::AdminSession;

/// The admin's decision for a member on an instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = AccountDecision)]
pub enum DecisionDto {
    /// Nothing decided: Starr does nothing for the member there.
    Undecided,
    /// Starr creates and manages an account.
    Create,
    /// An existing account is linked (adopted, password deferred).
    Linked,
    /// No account there; hands off.
    None,
}

impl From<Decision> for DecisionDto {
    fn from(d: Decision) -> Self {
        match d {
            Decision::Undecided => Self::Undecided,
            Decision::Create => Self::Create,
            Decision::Linked => Self::Linked,
            Decision::None => Self::None,
        }
    }
}

/// How Starr came to own an account.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = AccountOrigin)]
pub enum OriginDto {
    Provisioned,
    Adopted,
}

impl From<Origin> for OriginDto {
    fn from(o: Origin) -> Self {
        match o {
            Origin::Provisioned => Self::Provisioned,
            Origin::Adopted => Self::Adopted,
        }
    }
}

/// Which password an owned account uses.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = AccountPassword)]
pub enum PasswordDto {
    /// The member's Starr password.
    Starr,
    /// Still the member's old password (adopted, until they switch).
    Deferred,
    /// Adopted; the member asked to switch to the Starr password and the
    /// worker has not written it yet.
    Pending,
}

impl From<PasswordUse> for PasswordDto {
    fn from(p: PasswordUse) -> Self {
        match p {
            PasswordUse::Starr => Self::Starr,
            PasswordUse::Deferred => Self::Deferred,
            PasswordUse::Pending => Self::Pending,
        }
    }
}

/// The planned state of a member on an instance (one derivation, lesson
/// 16): every state has an action or an explanation in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = AccountState)]
pub enum StateDto {
    /// No decision yet; nothing happens.
    Undecided,
    /// Decided: no account there.
    NoAccount,
    /// The owned account is marked ignored.
    Ignored,
    /// Decided "create", but the member needs nothing now.
    NotNeeded,
    /// No complete account listing yet; nothing is created.
    WaitingForListing,
    /// An entitled category maps no library on the instance yet.
    WaitingForLibraries,
    /// Starr would create the account (with the member's password).
    ToCreate,
    /// Managed; in sync when there are no steps.
    Managed,
    /// Locked by the service after failed logins; never lifted by Starr.
    LockedByService,
    /// The name is taken by another account there.
    NameTaken,
    /// The owned account has admin rights there; hands off.
    Privileged,
    /// The owned account is no longer listed.
    Missing,
}

fn state_dto(state: &PlanState) -> StateDto {
    match state {
        PlanState::Undecided { .. } => StateDto::Undecided,
        PlanState::HandsOff {
            reason: HandsOffReason::NoAccount,
        } => StateDto::NoAccount,
        PlanState::HandsOff {
            reason: HandsOffReason::Ignored,
        } => StateDto::Ignored,
        PlanState::NotNeeded => StateDto::NotNeeded,
        PlanState::Waiting {
            reason: WaitReason::ListingIncomplete,
        } => StateDto::WaitingForListing,
        PlanState::Waiting {
            reason: WaitReason::LibrariesUnmapped,
        } => StateDto::WaitingForLibraries,
        PlanState::ToCreate => StateDto::ToCreate,
        PlanState::Managed => StateDto::Managed,
        PlanState::LockedByService => StateDto::LockedByService,
        PlanState::Conflict {
            conflict: Conflict::NameTaken { .. },
        } => StateDto::NameTaken,
        PlanState::Conflict {
            conflict: Conflict::Privileged,
        } => StateDto::Privileged,
        PlanState::Conflict {
            conflict: Conflict::Missing,
        } => StateDto::Missing,
    }
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = PlanAction)]
pub enum ActionDto {
    Create,
    Enable,
    Disable,
    SetLibraries,
    /// Write the member's Starr password (waits for the member to enter
    /// it; signs out the account's devices).
    SetPassword,
    /// Delete the account in the service: the member's deletion order is
    /// due (grace period over).
    Delete,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = PlanReason)]
pub enum ReasonDto {
    /// The member is active and entitled to categories there.
    Entitled,
    Suspended,
    /// Deletion order in its grace period.
    Retired,
    /// No category left on the instance.
    NoCategory,
    /// The library list differs from what the categories give.
    LibrariesDiffer,
    /// Starr created the account but no password reached it yet: it stays
    /// disabled until the member's password is written.
    PasswordMissing,
    /// The member changed their Starr password since it was written.
    PasswordChanged,
    /// The member asked to use their Starr password here.
    PasswordSwitch,
    /// The member's deletion order is due.
    DeletionDue,
    /// Starr locked the account with a random password (the service has no
    /// disable flag): only the member's password unlocks it.
    LockedByStarr,
}

impl From<Reason> for ReasonDto {
    fn from(r: Reason) -> Self {
        match r {
            Reason::Entitled => Self::Entitled,
            Reason::Suspended => Self::Suspended,
            Reason::Retired => Self::Retired,
            Reason::NoCategory => Self::NoCategory,
            Reason::LibrariesDiffer => Self::LibrariesDiffer,
            Reason::PasswordMissing => Self::PasswordMissing,
            Reason::PasswordChanged => Self::PasswordChanged,
            Reason::PasswordSwitch => Self::PasswordSwitch,
            Reason::DeletionDue => Self::DeletionDue,
            Reason::LockedByStarr => Self::LockedByStarr,
        }
    }
}

/// The DTO of a plan action.
pub(crate) const fn action_dto(action: &Action) -> ActionDto {
    match action {
        Action::Create { .. } => ActionDto::Create,
        Action::Enable => ActionDto::Enable,
        Action::Disable => ActionDto::Disable,
        Action::SetLibraries { .. } => ActionDto::SetLibraries,
        Action::SetPassword => ActionDto::SetPassword,
        Action::Delete => ActionDto::Delete,
    }
}

/// A library named by a step.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanLibrary {
    /// The service's grant ID.
    id: String,
    /// From the last library listing; absent if no longer listed.
    name: Option<String>,
}

/// One planned step.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    action: ActionDto,
    reason: ReasonDto,
    /// Disables, takes libraries away or deletes (counts against the cap).
    destructive: bool,
    /// Undoes a change made in the service since Starr's last write (a
    /// deviation).
    drift: bool,
    /// `create`: the name the account gets.
    username: Option<String>,
    /// `create`: its libraries; `setLibraries`: libraries added.
    add: Vec<PlanLibrary>,
    /// `setLibraries`: libraries taken away.
    remove: Vec<PlanLibrary>,
    /// `setLibraries`: the account sees every library now.
    from_all: bool,
}

fn step_dto(step: &Step, names: &HashMap<String, String>) -> PlanStep {
    let libs = |ids: &[String]| {
        ids.iter()
            .map(|id| PlanLibrary {
                name: names.get(id).cloned(),
                id: id.clone(),
            })
            .collect()
    };
    let (action, username, add, remove, from_all) = match &step.action {
        Action::Create {
            username,
            libraries,
        } => (
            ActionDto::Create,
            Some(username.clone()),
            libs(libraries),
            vec![],
            false,
        ),
        Action::Enable => (ActionDto::Enable, None, vec![], vec![], false),
        Action::Disable => (ActionDto::Disable, None, vec![], vec![], false),
        Action::SetPassword => (ActionDto::SetPassword, None, vec![], vec![], false),
        Action::Delete => (ActionDto::Delete, None, vec![], vec![], false),
        Action::SetLibraries {
            add,
            remove,
            from_all,
            ..
        } => (
            ActionDto::SetLibraries,
            None,
            libs(add),
            libs(remove),
            *from_all,
        ),
    };
    PlanStep {
        action,
        reason: step.reason.into(),
        destructive: step.destructive(),
        drift: step.drift,
        username,
        add,
        remove,
        from_all,
    }
}

/// Counts of a plan.
#[derive(Debug, Clone, Copy, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanCounts {
    create: u32,
    enable: u32,
    disable: u32,
    libraries: u32,
    /// Password writes that wait for the member's password.
    password: u32,
    /// Accounts deleted (due deletion orders).
    delete: u32,
    destructive: u32,
    /// Steps that undo a change made in the service.
    drift: u32,
    undecided: u32,
    conflicts: u32,
    waiting: u32,
}

impl From<Counts> for PlanCounts {
    fn from(c: Counts) -> Self {
        Self {
            create: c.create,
            enable: c.enable,
            disable: c.disable,
            libraries: c.libraries,
            password: c.password,
            delete: c.delete,
            destructive: c.destructive,
            drift: c.drift,
            undecided: c.undecided,
            conflicts: c.conflicts,
            waiting: c.waiting,
        }
    }
}

/// An account in a service, as far as a cell needs it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountRef {
    remote_id: String,
    /// The name there now (absent when no longer listed).
    username: Option<String>,
}

/// A member, as far as a plan entry needs it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemberRef {
    id: i64,
    username: String,
}

/// One member on one instance.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountCell {
    service_id: i64,
    decision: DecisionDto,
    /// The account Starr owns there.
    account: Option<AccountRef>,
    origin: Option<OriginDto>,
    password: Option<PasswordDto>,
    /// The name there when it was linked; shown as "named '…' there" when
    /// it differs from the member's username. When `account.username`
    /// differs from it, the account was renamed in the service since (D4:
    /// still managed, by its remote ID).
    linked_username: Option<String>,
    /// The owned account is not in the last complete listing(s): last seen
    /// then (D13 "missing since"). Absent while it is listed.
    missing_since: Option<DateTime<Utc>>,
    /// Since when and by whom (absent: decided by Starr, e.g. for a new
    /// member or an invitation that named the account).
    decided_at: Option<DateTime<Utc>>,
    decided_by: Option<String>,
    /// Undecided only: an unlinked account with the member's name. Only a
    /// suggestion; nothing is linked without confirmation.
    suggestion: Option<AccountRef>,
    /// From the last plan; absent until the instance was planned.
    state: Option<StateDto>,
    /// `nameTaken`: the account holding the name.
    conflict_account: Option<AccountRef>,
    /// Planned steps (executed only on instances switched live).
    steps: Vec<PlanStep>,
    /// Starr locked the owned account with a random password (a service
    /// without a disable flag, Navidrome): the member's password unlocks it.
    locked_by_starr: bool,
}

/// One row of the matrix.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MatrixMember {
    id: i64,
    username: String,
    display_name: String,
    status: MemberStatusDto,
    /// One per instance, in the order of `services`.
    cells: Vec<AccountCell>,
}

/// One column of the matrix.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MatrixService {
    id: i64,
    name: String,
    adapter_type: String,
    scanned_at: Option<DateTime<Utc>>,
    scan_complete: Option<bool>,
    /// When the stored plan was computed; absent: none yet.
    planned_at: Option<DateTime<Utc>>,
    /// The service has no disable flag (Navidrome): Starr disables by
    /// setting a random password, and only the member's next password
    /// enables the account again.
    disables_by_password: bool,
}

/// Members × consume instances with accounts.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountMatrix {
    services: Vec<MatrixService>,
    members: Vec<MatrixMember>,
}

/// The stored plan of one instance.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountPlan {
    service_id: i64,
    /// Whether account writes are on and what the next run does.
    writes: AccountWritesDto,
    /// Absent until the instance was planned.
    computed_at: Option<DateTime<Utc>>,
    /// Rests on a complete account listing.
    complete: bool,
    /// Names exactly this plan; confirming it sends it back.
    fingerprint: Option<String>,
    pub(crate) counts: PlanCounts,
    /// Every member, with state and steps.
    entries: Vec<PlanEntryDto>,
    /// Active admins without two-factor authentication (spec 0005 M10 1):
    /// while one exists, going live, resuming and approving are refused.
    admins_without_totp: Vec<String>,
}

/// The usernames of active admins without TOTP (spec 0005 M10 1).
pub(crate) async fn admins_without_totp(state: &AppState) -> Result<Vec<String>, ApiError> {
    Ok(starr_worker::provisioning::admins_without_totp(&state.db).await?)
}

/// An instance's plan with its name.
pub(crate) struct ServicePlan {
    pub(crate) service_name: String,
    pub(crate) plan: AccountPlan,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntryDto {
    member: MemberRef,
    state: StateDto,
    account: Option<AccountRef>,
    conflict_account: Option<AccountRef>,
    steps: Vec<PlanStep>,
}

/// One instance in a member's plan.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemberAccountPlan {
    service_id: i64,
    service_name: String,
    computed_at: Option<DateTime<Utc>>,
    complete: bool,
    cell: AccountCell,
}

/// What an admin decides.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecideRequest {
    decision: DecisionDto,
    /// Required for `linked`: the service's account ID.
    remote_id: Option<String>,
}

/// Everything about one instance the cells need.
struct InstanceView {
    id: i64,
    name: String,
    adapter_type: String,
    accounts: Vec<RemoteAccountRow>,
    rows: HashMap<i64, ServiceAccountRow>,
    plan: Option<(DateTime<Utc>, Plan)>,
    library_names: HashMap<String, String>,
    scanned_at: Option<DateTime<Utc>>,
    scan_complete: Option<bool>,
}

impl InstanceView {
    /// When an owned account was last seen, if the last complete listing
    /// did not contain it (or it is gone from the inventory).
    fn missing_since(&self, remote_id: &str) -> Option<DateTime<Utc>> {
        match self.accounts.iter().find(|a| a.remote_id == remote_id) {
            Some(a) if a.missing_runs > 0 || a.gone_since.is_some() => Some(a.last_seen),
            Some(_) => None,
            // Purged after it was gone for 30 days: since unknown.
            None => self.scanned_at,
        }
    }

    fn account_ref(&self, remote_id: &str) -> AccountRef {
        AccountRef {
            remote_id: remote_id.to_owned(),
            username: self
                .accounts
                .iter()
                .find(|a| a.remote_id == remote_id)
                .map(|a| a.username.clone()),
        }
    }

    fn observed(&self) -> Vec<Observed> {
        self.accounts
            .iter()
            .map(|r| Observed {
                remote_id: r.remote_id.clone(),
                username: r.username.clone(),
                enabled: r.enabled,
                privileged: r.privileged,
                locked_by_service: r.locked_by_service,
                libraries: r.libraries.clone(),
                gone: r.gone_since.is_some(),
                ignored: r.ignored_at.is_some(),
                locked_by_starr: r.locked_by_starr,
            })
            .collect()
    }

    /// The suggestion for an undecided member, computed from the current
    /// inventory (not the stored plan, so it never offers an account that
    /// was linked since).
    fn suggestion(&self, member: &MemberRecord) -> Option<AccountRef> {
        let observed = self.observed();
        let owned: HashSet<&str> = self
            .rows
            .values()
            .filter_map(|r| r.remote_id.as_deref())
            .collect();
        provisioning::suggested_account(&member.username, &observed, |id| owned.contains(id))
            .map(|a| self.account_ref(&a.remote_id))
    }

    fn cell(&self, member: &MemberRecord) -> AccountCell {
        let row = self.rows.get(&member.id);
        let decision = row.map_or(Decision::Undecided, |r| r.decision);
        let entry: Option<&PlanEntry> = self
            .plan
            .as_ref()
            .and_then(|(_, p)| p.entries.iter().find(|e| e.member_id == member.id));
        let conflict_account = entry.and_then(|e| match &e.state {
            PlanState::Conflict {
                conflict: Conflict::NameTaken { remote_id },
            } => Some(self.account_ref(remote_id)),
            _ => None,
        });
        AccountCell {
            service_id: self.id,
            decision: decision.into(),
            account: row
                .and_then(|r| r.remote_id.as_deref())
                .map(|id| self.account_ref(id)),
            origin: row.and_then(|r| r.origin).map(Into::into),
            password: row.and_then(|r| r.password).map(Into::into),
            linked_username: row.and_then(|r| r.linked_username.clone()),
            missing_since: row
                .and_then(|r| r.remote_id.as_deref())
                .and_then(|id| self.missing_since(id)),
            decided_at: row.map(|r| r.decided_at),
            decided_by: row.and_then(|r| r.decided_by.clone()),
            suggestion: (decision == Decision::Undecided)
                .then(|| self.suggestion(member))
                .flatten(),
            state: entry.map(|e| state_dto(&e.state)),
            conflict_account,
            steps: entry
                .map(|e| {
                    e.steps
                        .iter()
                        .map(|s| step_dto(s, &self.library_names))
                        .collect()
                })
                .unwrap_or_default(),
            locked_by_starr: row.and_then(|r| r.remote_id.as_deref()).is_some_and(|id| {
                self.accounts
                    .iter()
                    .any(|a| a.remote_id == id && a.locked_by_starr)
            }),
        }
    }
}

/// Parse a stored plan; a plan this binary cannot read (written by a newer
/// release) counts as none.
fn parse_plan(json: &str) -> Option<Plan> {
    match serde_json::from_str(json) {
        Ok(plan) => Some(plan),
        Err(err) => {
            tracing::warn!(error = %err, "stored account plan unreadable; ignored");
            None
        }
    }
}

async fn instance_views(state: &AppState) -> Result<Vec<InstanceView>, ApiError> {
    let services = state.db.list_services().await?;
    let mut out = Vec::new();
    for (id, name, _) in account_instances(&state.db).await? {
        let adapter_type = services
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.adapter_type.clone())
            .unwrap_or_default();
        out.push(instance_view(state, id, name, adapter_type).await?);
    }
    Ok(out)
}

async fn instance_view(
    state: &AppState,
    id: i64,
    name: String,
    adapter_type: String,
) -> Result<InstanceView, ApiError> {
    let scan = state.db.account_scan_state(id).await?;
    let plan = state
        .db
        .account_plan(id)
        .await?
        .and_then(|p| Some((p.computed_at, parse_plan(&p.plan)?)));
    Ok(InstanceView {
        id,
        name,
        adapter_type,
        accounts: state.db.remote_accounts(id).await?,
        rows: state
            .db
            .service_accounts(id)
            .await?
            .into_iter()
            .map(|r| (r.member_id, r))
            .collect(),
        plan,
        library_names: state
            .db
            .service_libraries(id)
            .await?
            .into_iter()
            .map(|l| (l.library.grant_id.unwrap_or(l.library.id), l.library.name))
            .collect(),
        scanned_at: scan.as_ref().and_then(|s| s.scanned_at),
        scan_complete: scan.and_then(|s| s.complete),
    })
}

/// The instance as an account instance (consume role, adapter with
/// accounts), or `accounts.unsupported`.
async fn account_instance(state: &AppState, id: i64) -> Result<InstanceView, ApiError> {
    let service = state
        .db
        .get_service(id)
        .await?
        .ok_or(ApiError::NotFound("services.not_found"))?;
    let (_, name, _) = account_instances(&state.db)
        .await?
        .into_iter()
        .find(|(i, _, _)| *i == id)
        .ok_or(ApiError::Validation("accounts.unsupported"))?;
    instance_view(state, id, name, service.adapter_type).await
}

async fn find_member(state: &AppState, id: i64) -> Result<MemberRecord, ApiError> {
    state
        .db
        .get_member(id)
        .await?
        .ok_or(ApiError::NotFound("member.not_found"))
}

fn matrix_member(member: &MemberRecord, views: &[InstanceView]) -> MatrixMember {
    MatrixMember {
        id: member.id,
        username: member.username.clone(),
        display_name: member.display_name.clone(),
        status: member.status.into(),
        cells: views.iter().map(|v| v.cell(member)).collect(),
    }
}

/// Queue `accounts.reconcile` after a change that affects the desired
/// state (entitlements, member status, mappings, roles) and wake the
/// worker. The job writes only to instances an admin switched live.
pub(crate) async fn accounts_changed(state: &AppState) -> Result<(), ApiError> {
    if account_instances(&state.db).await?.is_empty() && !state.db.has_account_plans().await? {
        return Ok(());
    }
    enqueue_reconcile(&state.db, Trigger::Manual).await?;
    state.wake.notify_one();
    Ok(())
}

/// Members × instances with decisions, suggestions and the planned state.
#[utoipa::path(get, path = "/api/v1/accounts/matrix", tag = "accounts",
    responses(
        (status = 200, body = AccountMatrix),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn account_matrix(
    _admin: AdminSession,
    State(state): State<AppState>,
) -> Result<Json<AccountMatrix>, ApiError> {
    let views = instance_views(&state).await?;
    let members = state.db.list_members(MemberFilter::default()).await?;
    Ok(Json(AccountMatrix {
        services: views
            .iter()
            .map(|v| MatrixService {
                id: v.id,
                name: v.name.clone(),
                adapter_type: v.adapter_type.clone(),
                scanned_at: v.scanned_at,
                scan_complete: v.scan_complete,
                planned_at: v.plan.as_ref().map(|(at, _)| *at),
                disables_by_password: adapters::account_rules(&v.adapter_type)
                    .is_some_and(|r| r.disable == DisableMode::ScramblePassword),
            })
            .collect(),
        members: members.iter().map(|m| matrix_member(m, &views)).collect(),
    }))
}

fn refusal_error(refusal: &LinkRefusal) -> ApiError {
    match refusal {
        LinkRefusal::NotFound => ApiError::NotFound("accounts.not_found"),
        LinkRefusal::Privileged => ApiError::Validation("accounts.privileged"),
        LinkRefusal::Ignored => ApiError::Validation("accounts.ignored"),
        LinkRefusal::Gone => ApiError::Validation("accounts.gone"),
        LinkRefusal::Taken { .. } => ApiError::Conflict("accounts.taken"),
        // Only from old invitations; an admin's own link is confirmed.
        LinkRefusal::Unconfirmed => ApiError::Forbidden,
    }
}

/// Store a decision, audit it and replan. Returns whether it changed.
async fn decide(
    state: &AppState,
    admin: &AdminSession,
    member: &MemberRecord,
    view: &InstanceView,
    change: DecisionChange<'_>,
) -> Result<bool, ApiError> {
    let now = state.clock.now();
    let by = &admin.0.subject.username;
    let outcome = state
        .db
        .decide_service_account(member.id, view.id, change, by, now)
        .await?;
    let (row, previous) = match outcome {
        DecideOutcome::Unchanged(_) => return Ok(false),
        DecideOutcome::Refused(refusal) => return Err(refusal_error(&refusal)),
        DecideOutcome::ProvisionedAccount => {
            return Err(ApiError::Conflict("accounts.provisioned"));
        }
        DecideOutcome::Changed { row, previous } => (row, previous),
    };
    let target = format!("{}:{}", view.name, member.username);
    if let Some(old) = previous.as_ref().and_then(|p| p.remote_id.as_deref())
        && row.remote_id.as_deref() != Some(old)
    {
        common::audit(
            state,
            &admin.0.subject,
            "account.unlink",
            Some(&target),
            Some(json!({ "serviceId": view.id, "remoteId": old })),
        )
        .await?;
    }
    if let Some(remote_id) = &row.remote_id
        && previous.as_ref().and_then(|p| p.remote_id.as_ref()) != Some(remote_id)
    {
        common::audit(
            state,
            &admin.0.subject,
            "account.link",
            Some(&target),
            Some(json!({
                "serviceId": view.id,
                "remoteId": remote_id,
                "name": row.linked_username,
            })),
        )
        .await?;
    } else if row.remote_id.is_none() {
        common::audit(
            state,
            &admin.0.subject,
            "account.decide",
            Some(&target),
            Some(json!({ "serviceId": view.id, "decision": row.decision.as_str() })),
        )
        .await?;
    }
    Ok(true)
}

/// Decide for a member on an instance: `undecided`, `create`, `none`, or
/// `linked` with the `remoteId` of an existing account (not privileged,
/// ignored, gone or owned by another member). Starr-side only: nothing is
/// written to the service; the plan is recomputed at once. Audited when it
/// changes something.
#[utoipa::path(put, path = "/api/v1/members/{id}/service-accounts/{serviceId}", tag = "accounts",
    params(
        ("id" = i64, Path, description = "Member id"),
        ("serviceId" = i64, Path, description = "Service id"),
    ),
    request_body = DecideRequest,
    responses(
        (status = 200, body = AccountCell),
        (status = 404, body = ErrorBody, description = "`member.not_found`, `services.not_found`, `accounts.not_found`"),
        (status = 409, body = ErrorBody, description = "`accounts.taken` (another member owns it), `accounts.provisioned` (Starr created the account; replacing it, D8, is not supported yet: delete the member or keep the account)"),
        (status = 422, body = ErrorBody, description = "`accounts.unsupported`, `accounts.remote_id_required`, `accounts.privileged`, `accounts.ignored`, `accounts.gone`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin), `auth.elevation_required` (linking adopts an account: re-authenticate first)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn decide_service_account(
    admin: AdminSession,
    State(state): State<AppState>,
    Path((id, service_id)): Path<(i64, i64)>,
    Json(body): Json<DecideRequest>,
) -> Result<Json<AccountCell>, ApiError> {
    if body.decision == DecisionDto::Linked {
        common::require_elevation(&state, &admin.0)?;
    }
    let member = find_member(&state, id).await?;
    let view = account_instance(&state, service_id).await?;
    let remote_id = body.remote_id.as_deref().map(str::trim);
    let change = match body.decision {
        DecisionDto::Undecided => DecisionChange::Undecided,
        DecisionDto::Create => DecisionChange::Create,
        DecisionDto::None => DecisionChange::NoAccount,
        DecisionDto::Linked => DecisionChange::Link {
            remote_id: remote_id
                .filter(|r| !r.is_empty())
                .ok_or(ApiError::Validation("accounts.remote_id_required"))?,
        },
    };
    if decide(&state, &admin, &member, &view, change).await? {
        // Wall clock, like the worker: stored plans never go back in time.
        replan(&state.db, Utc::now()).await?;
    }
    let view = account_instance(&state, service_id).await?;
    Ok(Json(view.cell(&member)))
}

/// Link every suggested account of a member's row (equal name, ignoring
/// case; only cells still undecided). One click for the common case; other
/// cells need their own decision (M3). Starr-side only.
#[utoipa::path(post, path = "/api/v1/members/{id}/service-accounts/confirm-suggestions", tag = "accounts",
    params(("id" = i64, Path, description = "Member id")),
    responses(
        (status = 200, body = MatrixMember),
        (status = 404, body = ErrorBody, description = "`member.not_found`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin), `auth.elevation_required`", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn confirm_suggestions(
    admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<MatrixMember>, ApiError> {
    common::require_elevation(&state, &admin.0)?;
    let member = find_member(&state, id).await?;
    let mut changed = false;
    for view in instance_views(&state).await? {
        let undecided = view
            .rows
            .get(&member.id)
            .is_none_or(|r| r.decision == Decision::Undecided);
        let Some(suggestion) = undecided.then(|| view.suggestion(&member)).flatten() else {
            continue;
        };
        let change = DecisionChange::Link {
            remote_id: &suggestion.remote_id,
        };
        // A cell taken meanwhile (another admin) stays undecided; the
        // others are still linked.
        match decide(&state, &admin, &member, &view, change).await {
            Ok(c) => changed |= c,
            Err(ApiError::Conflict(_) | ApiError::Validation(_) | ApiError::NotFound(_)) => {}
            Err(err) => return Err(err),
        }
    }
    if changed {
        replan(&state.db, Utc::now()).await?;
    }
    let views = instance_views(&state).await?;
    Ok(Json(matrix_member(&member, &views)))
}

/// The stored plan of one instance with every member's state and steps,
/// with whether account writes are on and what the next run does.
#[utoipa::path(get, path = "/api/v1/services/{id}/account-plan", tag = "accounts",
    params(("id" = i64, Path, description = "Service id")),
    responses(
        (status = 200, body = AccountPlan),
        (status = 404, body = ErrorBody, description = "`services.not_found`"),
        (status = 422, body = ErrorBody, description = "`accounts.unsupported`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn service_account_plan(
    _admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<AccountPlan>, ApiError> {
    Ok(Json(service_plan(&state, id).await?.plan))
}

/// The stored plan of an instance with accounts, with the write state.
pub(crate) async fn service_plan(state: &AppState, id: i64) -> Result<ServicePlan, ApiError> {
    let view = account_instance(state, id).await?;
    let stored = state.db.account_plan(id).await?;
    let account_writes = state
        .db
        .account_writes(id)
        .await?
        .ok_or(ApiError::NotFound("services.not_found"))?;
    let now = state.clock.now();
    // Tombstones whose accounts are still to delete are planned too.
    let members: HashMap<i64, String> = plan_members(&state.db, now)
        .await?
        .into_iter()
        .map(|m| (m.id, m.username))
        .collect();
    let admins_without_totp = admins_without_totp(state).await?;
    let recent = state.db.recent_destructive_writes(id, now).await?;
    let Some((computed_at, plan)) = &view.plan else {
        return Ok(ServicePlan {
            service_name: view.name.clone(),
            plan: AccountPlan {
                service_id: id,
                writes: writes_dto(&account_writes, None, recent, now),
                computed_at: None,
                complete: false,
                fingerprint: None,
                counts: PlanCounts::default(),
                entries: vec![],
                admins_without_totp,
            },
        });
    };
    let fingerprint = stored.map(|s| s.fingerprint);
    let entries = plan
        .entries
        .iter()
        .filter_map(|e| {
            // Members deleted since the plan was computed are left out.
            let username = members.get(&e.member_id)?.clone();
            let conflict_account = match &e.state {
                PlanState::Conflict {
                    conflict: Conflict::NameTaken { remote_id },
                } => Some(view.account_ref(remote_id)),
                _ => None,
            };
            Some(PlanEntryDto {
                member: MemberRef {
                    id: e.member_id,
                    username,
                },
                state: state_dto(&e.state),
                account: e.remote_id.as_deref().map(|r| view.account_ref(r)),
                conflict_account,
                steps: e
                    .steps
                    .iter()
                    .map(|s| step_dto(s, &view.library_names))
                    .collect(),
            })
        })
        .collect();
    Ok(ServicePlan {
        service_name: view.name.clone(),
        plan: AccountPlan {
            service_id: id,
            writes: writes_dto(
                &account_writes,
                fingerprint.as_deref().map(|f| (plan, f)),
                recent,
                now,
            ),
            computed_at: Some(*computed_at),
            complete: plan.complete,
            fingerprint,
            counts: plan.counts().into(),
            entries,
            admins_without_totp,
        },
    })
}

/// One member's decisions and planned steps on every instance with
/// accounts.
#[utoipa::path(get, path = "/api/v1/members/{id}/account-plan", tag = "accounts",
    params(("id" = i64, Path, description = "Member id")),
    responses(
        (status = 200, body = Vec<MemberAccountPlan>),
        (status = 404, body = ErrorBody, description = "`member.not_found`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn member_account_plan(
    _admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<MemberAccountPlan>>, ApiError> {
    let member = find_member(&state, id).await?;
    let views = instance_views(&state).await?;
    Ok(Json(
        views
            .iter()
            .map(|v| MemberAccountPlan {
                service_id: v.id,
                service_name: v.name.clone(),
                computed_at: v.plan.as_ref().map(|(at, _)| *at),
                complete: v.plan.as_ref().is_some_and(|(_, p)| p.complete),
                cell: v.cell(&member),
            })
            .collect(),
    ))
}
/// Check the accounts an invitation names (at most one per instance): each
/// on a consume instance with accounts, listed, not privileged, ignored or
/// gone, and not linked to a member. Returns them with their names.
pub(crate) async fn preset_accounts(
    state: &AppState,
    wanted: &[(i64, &str)],
    entitlements: &[Entitlement],
) -> Result<Vec<PresetAccount>, ApiError> {
    if wanted.is_empty() {
        return Ok(vec![]);
    }
    let enabled: Vec<Category> = state
        .db
        .list_categories()
        .await?
        .into_iter()
        .filter(|c| c.enabled)
        .map(|c| c.category)
        .collect();
    let all = account_instances(&state.db).await?;
    let instances: HashSet<i64> = all.iter().map(|(id, _, _)| *id).collect();
    // A named account the new member gets no category on would be planned
    // as disabled right after accepting (without preview): refused.
    let entitled_on = |id: i64| {
        all.iter()
            .find(|(i, _, _)| *i == id)
            .is_some_and(|(_, _, roles)| {
                roles
                    .iter()
                    .any(|(c, _)| libraries::entitled(entitlements, &enabled, *c))
            })
    };
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(wanted.len());
    for &(service_id, remote_id) in wanted {
        if !seen.insert(service_id) {
            return Err(ApiError::Validation(
                "invitations.accounts_duplicate_service",
            ));
        }
        if !instances.contains(&service_id) {
            return Err(if state.db.service_exists(service_id).await? {
                ApiError::Validation("accounts.unsupported")
            } else {
                ApiError::NotFound("services.not_found")
            });
        }
        if !entitled_on(service_id) {
            return Err(ApiError::Validation("invitations.accounts_not_entitled"));
        }
        let account = state
            .db
            .remote_accounts(service_id)
            .await?
            .into_iter()
            .find(|a| a.remote_id == remote_id)
            .ok_or(ApiError::NotFound("accounts.not_found"))?;
        if account.privileged {
            return Err(ApiError::Validation("accounts.privileged"));
        }
        if account.ignored_at.is_some() {
            return Err(ApiError::Validation("accounts.ignored"));
        }
        if account.gone_since.is_some() {
            return Err(ApiError::Validation("accounts.gone"));
        }
        if state
            .db
            .account_owner(service_id, remote_id)
            .await?
            .is_some()
        {
            return Err(ApiError::Conflict("accounts.taken"));
        }
        out.push(PresetAccount {
            service_id,
            remote_id: account.remote_id,
            username: Some(account.username),
            // Creating an invitation that names accounts needs elevation.
            confirmed: true,
        });
    }
    Ok(out)
}

/// Decisions for a member created just now (M3: members created after an
/// instance got accounts are provisioned automatically): link the accounts
/// the invitation named (password deferred), `create` on every other
/// consume instance with accounts. A named account that can no longer be
/// linked leaves the member undecided there, for the admin. Queues a
/// replan; nothing is written to any service. Runs after the member was
/// committed, so a failure is logged, not returned: without rows the member
/// is undecided everywhere, which is safe (Starr does nothing for them).
pub(crate) async fn new_member_accounts(
    state: &AppState,
    member_id: i64,
    username: &str,
    presets: &[PresetAccount],
) {
    if let Err(err) = record_new_member_accounts(state, member_id, username, presets).await {
        tracing::error!(
            member = member_id,
            code = err.code(),
            "initial account decisions not stored; member stays undecided"
        );
    }
}

async fn record_new_member_accounts(
    state: &AppState,
    member_id: i64,
    username: &str,
    presets: &[PresetAccount],
) -> Result<(), ApiError> {
    let instances = account_instances(&state.db).await?;
    if instances.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = instances.iter().map(|(id, _, _)| *id).collect();
    // Instances that lost their accounts since the invitation was made.
    let presets: Vec<PresetAccount> = presets
        .iter()
        .filter(|p| ids.contains(&p.service_id))
        .cloned()
        .collect();
    let now = state.clock.now();
    let refused = state
        .db
        .initial_service_accounts(member_id, &ids, &presets, now)
        .await?;
    for preset in &presets {
        let name = instances
            .iter()
            .find(|(id, _, _)| *id == preset.service_id)
            .map_or("", |(_, name, _)| name.as_str());
        let target = format!("{name}:{username}");
        let refusal = refused.iter().find(|(p, _)| p == preset).map(|(_, r)| r);
        let (action, details) = match refusal {
            None => (
                "account.link",
                json!({ "serviceId": preset.service_id, "remoteId": preset.remote_id, "via": "invitation" }),
            ),
            Some(refusal) => (
                "account.link_refused",
                json!({
                    "serviceId": preset.service_id,
                    "remoteId": preset.remote_id,
                    "via": "invitation",
                    "reason": format!("{refusal:?}"),
                }),
            ),
        };
        let details = details.to_string();
        state
            .db
            .audit(
                &AuditEntry {
                    actor_kind: ActorKind::System,
                    actor_name: None,
                    action,
                    target: Some(&target),
                    details: Some(&details),
                },
                now,
            )
            .await?;
    }
    accounts_changed(state).await
}
