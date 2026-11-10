//! Account provisioning planner (spec 0005 D3, slice 3): from the desired
//! state (members × entitlements × the libraries each category maps on an
//! instance) and what the instance's last account scan observed, decide per
//! member what Starr would do there and why. Pure: no IO, no clock.
//!
//! Slice 3 computed and showed plans; slice 4 executes them on instances an
//! admin switched live, through [`gate`] (safe mode, M8, D15). Rules:
//! - an **undecided** member gets no step, ever (M3): only an admin's
//!   decision (link / create / none) lets Starr act for an existing member;
//! - ownership is the stored remote ID, never the name (D4); an equal name
//!   is only a suggestion for undecided members;
//! - privileged and ignored accounts get no step (D2, D7);
//! - nothing is created from an incomplete listing (the name could be
//!   taken by an account the listing missed), and a missing account is a
//!   conflict, never a reason to create a second one (lesson 8);
//! - libraries are always an explicit, non-empty list; a member without a
//!   category there is disabled, never given an empty list (M4, D11);
//! - a lockout set by the service is never lifted (D10);
//! - an active member whose entitled category maps no library on the
//!   instance waits: Starr does not shrink or disable an account because
//!   the admin has not finished mapping (suspension still disables);
//! - steps for owned accounts are planned from the last stored inventory
//!   even when the last listing was incomplete; such a plan is marked not
//!   complete and is never executed ([`gate`]);
//! - an owned account is deleted only when the member's deletion order is
//!   due and Starr created it (D16, slice 6; adopted accounts are disabled
//!   and released), never a privileged, ignored or hands-off one; the
//!   step carries the only way to get a [`DeletePlan`] ([`delete_plan`]);
//! - a step that undoes a change made in the service since Starr's last
//!   write (or since a live run found the account in sync) is marked as a
//!   deviation (`drift`, D13): it is still applied, and shown as such;
//! - on a service without a disable flag ([`DisableMode::ScramblePassword`],
//!   Navidrome, D9) "disabled" is Starr's own record that it set a random
//!   password nobody knows (`locked_by_starr`): the service still reports
//!   the account as enabled, a disable is a scramble, and there is no
//!   enable step: only the member's password unlocks the account again,
//!   so an active member's locked account gets a password step
//!   ([`Reason::LockedByStarr`]) that waits for their next handoff.

use std::collections::{BTreeSet, HashSet};

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::accounts::{DeletePlan, DisableMode};
use crate::members::{Category, MemberStatus};

/// The admin's decision for one member on one instance (M3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    /// Nothing decided yet: Starr does nothing for the member there.
    Undecided,
    /// Starr creates (and then manages) an account.
    Create,
    /// An existing account is linked and managed (password deferred, D6).
    Linked,
    /// The member gets no account there; Starr keeps its hands off.
    None,
}

impl Decision {
    pub const ALL: [Self; 4] = [Self::Undecided, Self::Create, Self::Linked, Self::None];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Undecided => "undecided",
            Self::Create => "create",
            Self::Linked => "linked",
            Self::None => "none",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.as_str() == value)
    }
}

/// How a managed account came to Starr (D4: the evidence of ownership).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// Created by Starr.
    Provisioned,
    /// An existing account an admin linked.
    Adopted,
}

impl Origin {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Provisioned => "provisioned",
            Self::Adopted => "adopted",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Provisioned, Self::Adopted]
            .into_iter()
            .find(|o| o.as_str() == value)
    }
}

/// Which password a managed account uses (ADR 0010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PasswordUse {
    /// The member's Starr password.
    Starr,
    /// Adopted: still the member's old password until they switch (M2).
    Deferred,
    /// Adopted, and the member asked to switch to the Starr password (M2):
    /// the old password stays until the worker has written the new one.
    Pending,
}

impl PasswordUse {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starr => "starr",
            Self::Deferred => "deferred",
            Self::Pending => "pending",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Starr, Self::Deferred, Self::Pending]
            .into_iter()
            .find(|p| p.as_str() == value)
    }
}

/// What an owned account's password needs (ADR 0010, D12): written only
/// when due, never periodically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PasswordNeed {
    /// Nothing: the account has the member's current Starr password, or
    /// keeps the member's old one (adopted, deferred), or there is no
    /// account yet.
    #[default]
    Nothing,
    /// The account has an older Starr password: the member changed theirs
    /// since it was written.
    Changed,
    /// Adopted, and the member asked to switch to the Starr password (M2).
    Switch,
    /// Starr created the account but never wrote a password into it (a
    /// create that did not finish: Emby creates accounts active with an
    /// empty password). Never enabled until a password is written.
    Missing,
}

impl PasswordNeed {
    /// The need of an account that uses `password` with generation
    /// `written` written into it, for a member at `generation`.
    #[must_use]
    pub fn of(password: Option<PasswordUse>, written: Option<i64>, generation: i64) -> Self {
        match (password, written) {
            (Some(PasswordUse::Pending), _) => Self::Switch,
            (Some(PasswordUse::Starr), None) => Self::Missing,
            (Some(PasswordUse::Starr), Some(w)) if w != generation => Self::Changed,
            _ => Self::Nothing,
        }
    }
}

/// What Starr last left an owned account as: the read-back of its own
/// last write, or what a complete live run found in sync. A difference
/// later was made in the service (drift, D13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    pub enabled: bool,
    /// `None`: every library; otherwise the grant IDs.
    pub libraries: Option<Vec<String>>,
}

/// Starr's record for one member on one instance; `None` means undecided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub decision: Decision,
    /// The account Starr owns there, once there is one.
    pub remote_id: Option<String>,
    /// What the owned account's password needs.
    pub password: PasswordNeed,
    /// The baseline for drift; `None` until Starr wrote to the account or a
    /// live run found it in sync.
    pub expected: Option<Expected>,
    /// How Starr came to own the account: only accounts Starr created are
    /// deleted when the member's deletion order is due (D16, maintainer
    /// 2026-11-03); adopted ones are disabled and then released.
    pub origin: Option<Origin>,
}

/// One account from the instance's inventory (`remote_account`).
#[allow(clippy::struct_excessive_bools)] // the inventory row's flags
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub remote_id: String,
    pub username: String,
    pub enabled: bool,
    pub privileged: bool,
    pub locked_by_service: bool,
    /// `None`: every library; otherwise the grant IDs.
    pub libraries: Option<Vec<String>>,
    /// Missing from two complete listings (D7).
    pub gone: bool,
    /// Marked ignored by an admin (D7).
    pub ignored: bool,
    /// Starr disabled the account by setting a random password nobody
    /// knows (D9, services with [`DisableMode::ScramblePassword`]): it is
    /// disabled until a member's password is written again. Always `false`
    /// on services with a disable flag.
    pub locked_by_starr: bool,
}

/// What Starr knows about the instance.
#[derive(Debug, Clone, Copy)]
pub struct Instance<'a> {
    /// The last account scan succeeded with a provably complete listing.
    pub listing_complete: bool,
    /// How the service disables an account (its adapter's
    /// `AccountRules::disable`).
    pub disable: DisableMode,
    pub accounts: &'a [Observed],
}

/// What one member should have on the instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    pub member_id: i64,
    /// Starr's username, also the name a created account gets (D18).
    pub username: String,
    pub status: MemberStatus,
    /// Grant IDs of the libraries the member gets there
    /// (`libraries::desired`); empty: no category there.
    pub libraries: Vec<String>,
    /// Categories the member is entitled to whose consume role on the
    /// instance maps no library yet.
    pub unmapped: Vec<Category>,
    /// The member's deletion order is due (grace period over, D16): owned
    /// accounts are deleted. Only for retired members.
    pub deletion_due: bool,
}

/// Why a member waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WaitReason {
    /// No complete account listing: a name could be taken unseen.
    ListingIncomplete,
    /// An entitled category maps no library on the instance yet.
    LibrariesUnmapped,
}

/// A situation only an admin can resolve ("needs you").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Conflict {
    /// Starr would create the account, but the name is taken there.
    #[serde(rename_all = "camelCase")]
    NameTaken { remote_id: String },
    /// The owned account has administrator rights in the service (D2).
    Privileged,
    /// The owned account is not listed any more (gone or never seen).
    Missing,
}

/// Why Starr keeps its hands off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HandsOffReason {
    /// The admin decided the member gets no account there.
    NoAccount,
    /// The owned account is marked ignored.
    Ignored,
}

/// The state of one member on one instance as admins see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum State {
    /// Nothing decided: no step. `suggestion` is an unlinked account with
    /// the member's name (never privileged, ignored or gone).
    #[serde(rename_all = "camelCase")]
    Undecided {
        suggestion: Option<String>,
    },
    HandsOff {
        reason: HandsOffReason,
    },
    /// Decided "create", but the member needs nothing there now (not
    /// active, or no category).
    NotNeeded,
    Waiting {
        reason: WaitReason,
    },
    /// Starr would create the account (once the member's password is at
    /// hand, ADR 0010).
    ToCreate,
    /// Starr manages the account; in sync when there are no steps.
    Managed,
    /// Managed, but locked by the service after failed logins (D10).
    LockedByService,
    Conflict {
        conflict: Conflict,
    },
}

/// What Starr would do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Action {
    /// Create the account with this name and exactly these libraries.
    Create {
        username: String,
        libraries: Vec<String>,
    },
    Enable,
    Disable,
    /// Change the library list to the desired one. `from_all`: the
    /// account currently sees every library.
    #[serde(rename_all = "camelCase")]
    SetLibraries {
        /// The full desired list (never empty); `add` and `remove` are the
        /// difference to what the account had, for display. Plans stored
        /// before slice 4 lack it and are never executed (a fresh plan is
        /// computed before every run).
        #[serde(default)]
        libraries: Vec<String>,
        add: Vec<String>,
        remove: Vec<String>,
        from_all: bool,
    },
    /// Write the member's Starr password into the account (signs out its
    /// devices). Needs the member's password: executed only by
    /// `accounts.password` from a handoff (ADR 0010), never by a plan run.
    SetPassword,
    /// Delete the account in the service (D16): only for a member whose
    /// deletion order is due. Destructive; counts against the cap.
    Delete,
}

/// Why Starr would do it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Reason {
    /// The member is active and entitled to categories there.
    Entitled,
    /// The member is suspended.
    Suspended,
    /// The member is being deleted (deletion order in its grace period).
    Retired,
    /// The member has no category on the instance any more.
    NoCategory,
    /// The library list differs from what the member's categories give.
    LibrariesDiffer,
    /// Starr created the account but no password reached it yet: it stays
    /// disabled until the member's password is written (ADR 0010).
    PasswordMissing,
    /// The member changed their Starr password since it was written.
    PasswordChanged,
    /// The member asked to use their Starr password here (M2).
    PasswordSwitch,
    /// The member's deletion order is due: the grace period is over (D16).
    DeletionDue,
    /// Starr disabled the account with a random password (a service
    /// without a disable flag, D9): only the member's password unlocks it.
    LockedByStarr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub action: Action,
    pub reason: Reason,
    /// The step undoes a change made in the service since Starr's last
    /// write (D13: a deviation, shown to admins).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub drift: bool,
}

impl Step {
    #[must_use]
    pub const fn new(action: Action, reason: Reason) -> Self {
        Self {
            action,
            reason,
            drift: false,
        }
    }

    /// Disabling, taking libraries away or deleting (counts against the
    /// cap, D15).
    #[must_use]
    pub fn destructive(&self) -> bool {
        match &self.action {
            Action::Disable | Action::Delete => true,
            Action::SetLibraries {
                remove, from_all, ..
            } => *from_all || !remove.is_empty(),
            Action::Create { .. } | Action::Enable | Action::SetPassword => false,
        }
    }
}

/// The permission to delete the account of `entry` for `step`: only a
/// `Delete` step planned because the member's deletion order is due, for
/// the account the entry owns (D16, lesson 27).
#[must_use]
pub fn delete_plan(entry: &PlanEntry, step: &Step) -> Option<DeletePlan> {
    let remote_id = entry.remote_id.as_ref()?;
    let planned = entry.steps.iter().any(|s| s == step);
    (planned && step.action == Action::Delete && step.reason == Reason::DeletionDue)
        .then(|| DeletePlan::new(remote_id.clone()))
}

/// One member on one instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntry {
    pub member_id: i64,
    pub state: State,
    /// The account Starr owns there, if any.
    pub remote_id: Option<String>,
    pub steps: Vec<Step>,
}

/// The plan for one instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// Based on a complete listing; a plan that is not complete never
    /// creates, and slice 4 will never go live on one.
    pub complete: bool,
    /// One entry per member, in the order given.
    pub entries: Vec<PlanEntry>,
}

/// Step and state counts of a plan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Counts {
    pub create: u32,
    pub enable: u32,
    pub disable: u32,
    pub libraries: u32,
    /// Password writes due (each waits for the member's password).
    pub password: u32,
    /// Accounts deleted (due deletion orders).
    pub delete: u32,
    /// Steps that disable, take libraries away or delete.
    pub destructive: u32,
    /// Steps that undo a change made in the service (D13).
    pub drift: u32,
    pub undecided: u32,
    pub conflicts: u32,
    pub waiting: u32,
}

impl Plan {
    #[must_use]
    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for entry in &self.entries {
            match entry.state {
                State::Undecided { .. } => c.undecided += 1,
                State::Conflict { .. } => c.conflicts += 1,
                State::Waiting { .. } => c.waiting += 1,
                _ => {}
            }
            for step in &entry.steps {
                match step.action {
                    Action::Create { .. } => c.create += 1,
                    Action::Enable => c.enable += 1,
                    Action::Disable => c.disable += 1,
                    Action::SetLibraries { .. } => c.libraries += 1,
                    Action::SetPassword => c.password += 1,
                    Action::Delete => c.delete += 1,
                }
                if step.destructive() {
                    c.destructive += 1;
                }
                if step.drift {
                    c.drift += 1;
                }
            }
        }
        c
    }
}

impl Action {
    /// Whether a plan run (`accounts.reconcile`) executes this step.
    /// Creating and writing a password need the member's password (ADR
    /// 0010): `accounts.password` executes them from a handoff, through the
    /// same gate; without one they are shown and wait.
    #[must_use]
    pub const fn executable(&self) -> bool {
        !self.needs_password()
    }

    /// Whether the step needs the member's password (create, password).
    #[must_use]
    pub const fn needs_password(&self) -> bool {
        matches!(self, Self::Create { .. } | Self::SetPassword)
    }
}

impl Plan {
    /// Steps a live run would execute now, and how many of them are
    /// destructive.
    #[must_use]
    pub fn executable_steps(&self) -> (u32, u32) {
        let mut steps = 0u32;
        let mut destructive = 0u32;
        for step in self.entries.iter().flat_map(|e| &e.steps) {
            if step.action.executable() {
                steps += 1;
                destructive += u32::from(step.destructive());
            }
        }
        (steps, destructive)
    }
}

/// Destructive steps (disable, libraries taken away, delete) executed per
/// instance within [`CAP_WINDOW`] without an admin's approval (D15). The
/// spec counts per run; the rolling window keeps a series of small runs
/// (six single disables in a row) from passing the cap one by one
/// (maintainer's request in slice 6, accepted 2026-11-03; see spec D15).
pub const DESTRUCTIVE_CAP: u32 = 5;

/// The window over which executed destructive steps count against the cap.
pub const CAP_WINDOW: TimeDelta = TimeDelta::hours(1);

/// How long a complete live run keeps an instance "fresh" (M8 A): after an
/// hour without one, the next plan that writes waits for confirmation, so
/// an emergency change made in the service meanwhile is not silently
/// undone. The same hour bounds how old a confirmed plan and an approval
/// may be.
pub const FRESH_FOR: TimeDelta = TimeDelta::hours(1);

/// Whether an admin switched account writes on for an instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WritesMode {
    /// Never switched live: plan only (the default for every instance).
    PlanOnly,
    /// Live, but an admin paused the writes: plan only.
    Paused,
    Live,
}

/// An admin's confirmation of one plan (going live, resuming, approving a
/// held plan): it names the plan's fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Approval<'a> {
    pub fingerprint: &'a str,
    pub at: DateTime<Utc>,
}

/// Why a live plan waits for an admin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum HoldReason {
    /// No complete live run in the last hour (M8 A).
    NoRecentRun,
    /// More destructive steps than the cap (D15): this plan's
    /// `destructive` steps plus the `recent` ones already executed on the
    /// instance within [`CAP_WINDOW`].
    #[serde(rename_all = "camelCase")]
    OverCap {
        destructive: u32,
        cap: u32,
        #[serde(default)]
        recent: u32,
    },
}

/// What a run may do with a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Gate {
    /// Account writes are off for the instance: plan only.
    PlanOnly,
    /// Writes paused by an admin: plan only.
    Paused,
    /// The plan rests on an incomplete listing: never executed.
    Incomplete,
    /// Waits for an admin's approval of exactly this plan.
    Hold { reason: HoldReason },
    /// Execute the plan's executable steps (none is fine: the run still
    /// counts as a complete live run).
    Execute,
}

/// Everything [`gate`] decides on.
#[derive(Debug, Clone, Copy)]
pub struct GateInput<'a> {
    pub mode: WritesMode,
    pub plan: &'a Plan,
    pub fingerprint: &'a str,
    /// The last complete live run of the instance.
    pub last_live_run: Option<DateTime<Utc>>,
    pub approval: Option<Approval<'a>>,
    /// Destructive steps executed on the instance within [`CAP_WINDOW`].
    pub recent_destructive: u32,
    pub now: DateTime<Utc>,
}

/// Whether an approval names this plan and is at most [`FRESH_FOR`] old.
#[must_use]
pub fn approves(approval: Option<Approval<'_>>, fingerprint: &str, now: DateTime<Utc>) -> bool {
    approval.is_some_and(|a| a.fingerprint == fingerprint && now - a.at <= FRESH_FOR)
}

/// Safe mode (M8, D15): decide whether a run executes a plan. Plan only
/// unless the instance is live and not paused; never on an incomplete
/// plan; a plan without executable steps runs (and refreshes the live
/// run); an approval of exactly this plan lets it run; otherwise a plan
/// whose destructive steps, together with those executed within the last
/// [`CAP_WINDOW`], exceed the cap, or after an hour without a complete
/// live run, waits for approval.
#[must_use]
pub fn gate(input: &GateInput<'_>) -> Gate {
    match input.mode {
        WritesMode::PlanOnly => return Gate::PlanOnly,
        WritesMode::Paused => return Gate::Paused,
        WritesMode::Live => {}
    }
    if !input.plan.complete {
        return Gate::Incomplete;
    }
    let (steps, destructive) = input.plan.executable_steps();
    if steps == 0 || approves(input.approval, input.fingerprint, input.now) {
        return Gate::Execute;
    }
    if destructive > 0 && destructive.saturating_add(input.recent_destructive) > DESTRUCTIVE_CAP {
        let reason = HoldReason::OverCap {
            destructive,
            cap: DESTRUCTIVE_CAP,
            recent: input.recent_destructive,
        };
        return Gate::Hold { reason };
    }
    let fresh = input
        .last_live_run
        .is_some_and(|at| input.now - at <= FRESH_FOR);
    if !fresh {
        let reason = HoldReason::NoRecentRun;
        return Gate::Hold { reason };
    }
    Gate::Execute
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// The account suggested for an undecided member (M3): equal name ignoring
/// case, not privileged, ignored or gone, and not owned by anyone
/// (`owned` tells whether a remote ID is).
#[must_use]
pub fn suggested_account<'a>(
    username: &str,
    accounts: &'a [Observed],
    owned: impl Fn(&str) -> bool,
) -> Option<&'a Observed> {
    accounts.iter().find(|a| {
        !a.privileged
            && !a.ignored
            && !a.gone
            && !owned(&a.remote_id)
            && same_name(&a.username, username)
    })
}

/// Plan one instance: one entry per member in `members` (with Starr's link
/// for the member there, `None` = undecided).
#[must_use]
pub fn plan(instance: &Instance<'_>, members: &[(Wanted, Option<Link>)]) -> Plan {
    let owned: HashSet<&str> = members
        .iter()
        .filter_map(|(_, link)| link.as_ref()?.remote_id.as_deref())
        .collect();
    let entries = members
        .iter()
        .map(|(wanted, link)| {
            let link = link.as_ref().filter(|l| l.decision != Decision::Undecided);
            let (state, remote_id, steps) = match link {
                None => {
                    let suggestion = suggested_account(&wanted.username, instance.accounts, |id| {
                        owned.contains(id)
                    })
                    .map(|a| a.remote_id.clone());
                    (State::Undecided { suggestion }, None, vec![])
                }
                Some(l) if l.decision == Decision::None => {
                    let reason = HandsOffReason::NoAccount;
                    (State::HandsOff { reason }, None, vec![])
                }
                Some(l) => match l.remote_id.as_deref() {
                    None => {
                        let (state, steps) = to_create(instance, wanted);
                        (state, None, steps)
                    }
                    Some(remote_id) => {
                        let (state, steps) = owned_account(instance, wanted, remote_id, l);
                        (state, Some(remote_id.to_owned()), steps)
                    }
                },
            };
            PlanEntry {
                member_id: wanted.member_id,
                state,
                remote_id,
                steps,
            }
        })
        .collect();
    Plan {
        complete: instance.listing_complete,
        entries,
    }
}

fn needs_account(wanted: &Wanted) -> bool {
    wanted.status == MemberStatus::Active && !wanted.libraries.is_empty()
}

/// Decided "create", nothing created yet.
fn to_create(instance: &Instance<'_>, wanted: &Wanted) -> (State, Vec<Step>) {
    if wanted.status != MemberStatus::Active {
        return (State::NotNeeded, vec![]);
    }
    if !wanted.unmapped.is_empty() {
        let reason = WaitReason::LibrariesUnmapped;
        return (State::Waiting { reason }, vec![]);
    }
    if !needs_account(wanted) {
        return (State::NotNeeded, vec![]);
    }
    if !instance.listing_complete {
        let reason = WaitReason::ListingIncomplete;
        return (State::Waiting { reason }, vec![]);
    }
    if let Some(taken) = instance
        .accounts
        .iter()
        .find(|a| !a.gone && same_name(&a.username, &wanted.username))
    {
        let conflict = Conflict::NameTaken {
            remote_id: taken.remote_id.clone(),
        };
        return (State::Conflict { conflict }, vec![]);
    }
    let step = Step::new(
        Action::Create {
            username: wanted.username.clone(),
            libraries: wanted.libraries.clone(),
        },
        Reason::Entitled,
    );
    (State::ToCreate, vec![step])
}

/// An account Starr owns (linked or created).
fn owned_account(
    instance: &Instance<'_>,
    wanted: &Wanted,
    remote_id: &str,
    link: &Link,
) -> (State, Vec<Step>) {
    let password = link.password;
    let expected = link.expected.as_ref();
    let Some(account) = instance
        .accounts
        .iter()
        .find(|a| a.remote_id == remote_id && !a.gone)
    else {
        let conflict = Conflict::Missing;
        return (State::Conflict { conflict }, vec![]);
    };
    if account.privileged {
        let conflict = Conflict::Privileged;
        return (State::Conflict { conflict }, vec![]);
    }
    if account.ignored {
        let reason = HandsOffReason::Ignored;
        return (State::HandsOff { reason }, vec![]);
    }
    // Unfinished mapping never shrinks or disables an active member's
    // account; suspension and deletion disable it all the same (M5).
    if wanted.status == MemberStatus::Active && !wanted.unmapped.is_empty() {
        let reason = WaitReason::LibrariesUnmapped;
        return (State::Waiting { reason }, vec![]);
    }
    // Without a disable flag the service reports every account as enabled;
    // "disabled" is Starr's record of the random password it set (D9).
    let scramble = instance.disable == DisableMode::ScramblePassword;
    let enabled = if scramble {
        !account.locked_by_starr
    } else {
        account.enabled
    };
    let state = if account.locked_by_service {
        State::LockedByService
    } else {
        State::Managed
    };
    if wanted.deletion_due && wanted.status == MemberStatus::Retired {
        // D16 (maintainer 2026-11-03): an account Starr created goes,
        // enabled or not; an adopted one is only disabled (and released
        // afterwards), never deleted.
        let steps = if link.origin == Some(Origin::Provisioned) {
            vec![Step::new(Action::Delete, Reason::DeletionDue)]
        } else if enabled {
            vec![Step::new(Action::Disable, Reason::DeletionDue)]
        } else {
            vec![]
        };
        return (state, steps);
    }
    let mut steps = Vec::new();
    if needs_account(wanted) {
        if password == PasswordNeed::Missing {
            // No password ever reached the account (Emby: an empty one):
            // it stays disabled until the member's password is written.
            if enabled {
                steps.push(Step::new(Action::Disable, Reason::PasswordMissing));
            }
        } else if !enabled && !account.locked_by_service && !scramble {
            // Without a flag there is nothing to enable: the password step
            // below unlocks the account.
            steps.push(Step {
                drift: expected.is_some_and(|e| e.enabled),
                ..Step::new(Action::Enable, Reason::Entitled)
            });
        }
        if let Some(action) = library_change(account.libraries.as_deref(), &wanted.libraries) {
            // Starr had granted exactly these libraries: changed there.
            let drift = expected
                .and_then(|e| e.libraries.as_deref())
                .is_some_and(|had| same_set(had, &wanted.libraries));
            steps.push(Step {
                drift,
                ..Step::new(action, Reason::LibrariesDiffer)
            });
        }
        let reason = match password {
            _ if scramble && account.locked_by_starr => Some(Reason::LockedByStarr),
            PasswordNeed::Nothing => None,
            PasswordNeed::Changed => Some(Reason::PasswordChanged),
            PasswordNeed::Switch => Some(Reason::PasswordSwitch),
            PasswordNeed::Missing => Some(Reason::PasswordMissing),
        };
        if let Some(reason) = reason {
            // Never for a suspended member (D9): only here, active.
            steps.push(Step::new(Action::SetPassword, reason));
        }
    } else if enabled {
        // Libraries stay as they are: never an empty list (D11).
        let reason = match wanted.status {
            MemberStatus::Retired => Reason::Retired,
            MemberStatus::Suspended => Reason::Suspended,
            MemberStatus::Active => Reason::NoCategory,
        };
        // Starr had disabled it: enabled again in the service. Without a
        // flag nothing in the service can show that (a password changed
        // there is not detected, D12).
        steps.push(Step {
            drift: !scramble && expected.is_some_and(|e| !e.enabled),
            ..Step::new(Action::Disable, reason)
        });
    }
    (state, steps)
}

fn same_set(a: &[String], b: &[String]) -> bool {
    a.iter().collect::<BTreeSet<_>>() == b.iter().collect::<BTreeSet<_>>()
}

/// How a member sees one instance in "My services" (M9), in member words.
/// Never remote IDs, errors, libraries or other people.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MemberView {
    /// The account works with the member's Starr password.
    Ready,
    /// Starr needs the member's password once ("activate with your
    /// password"): a new account, or a password change not applied yet.
    Activate,
    /// Adopted: the account still uses the member's old password; "use my
    /// Starr password here" switches it (M2).
    OldPassword,
    /// Starr is working on it, or an admin has to act first.
    SettingUp,
    /// The member is suspended: the account is (or will be) disabled.
    Suspended,
}

/// Everything [`member_view`] decides on for one member on one instance.
#[derive(Debug, Clone, Copy)]
pub struct MemberViewInput<'a> {
    pub status: MemberStatus,
    /// The member's entry in the instance's last plan.
    pub entry: &'a PlanEntry,
    /// Which password the owned account uses.
    pub password: Option<PasswordUse>,
    /// Account writes are live (not plan only, not paused): only then can
    /// the member's password do anything now.
    pub live: bool,
    /// The member has a password handoff in flight.
    pub handoff: bool,
}

/// The member's view of one instance (one derivation, lesson 16); `None`
/// when the instance is not shown: nothing decided for the member yet,
/// hands off, or nothing needed there.
#[must_use]
pub fn member_view(input: &MemberViewInput<'_>) -> Option<MemberView> {
    let entry = input.entry;
    match input.status {
        MemberStatus::Retired => return None,
        MemberStatus::Suspended => {
            return entry.remote_id.is_some().then_some(MemberView::Suspended);
        }
        MemberStatus::Active => {}
    }
    let needs_password = entry.steps.iter().any(|s| s.action.needs_password());
    let waiting = if input.handoff || !input.live {
        MemberView::SettingUp
    } else {
        MemberView::Activate
    };
    match &entry.state {
        State::Undecided { .. } | State::HandsOff { .. } | State::NotNeeded => None,
        State::Waiting { .. } | State::Conflict { .. } | State::LockedByService => {
            Some(MemberView::SettingUp)
        }
        State::ToCreate => Some(waiting),
        State::Managed => Some(match input.password {
            // Locked by Starr with a random password (no disable flag): the
            // member's password unlocks it, whatever it used before.
            _ if entry
                .steps
                .iter()
                .any(|s| s.reason == Reason::LockedByStarr) =>
            {
                waiting
            }
            // A switch on its way, or one whose handoff expired (still the
            // old password).
            Some(PasswordUse::Pending) if input.handoff => MemberView::SettingUp,
            Some(PasswordUse::Pending | PasswordUse::Deferred) => MemberView::OldPassword,
            _ if needs_password => waiting,
            _ if entry
                .steps
                .iter()
                .any(|s| matches!(s.action, Action::Enable)) =>
            {
                MemberView::SettingUp
            }
            _ => MemberView::Ready,
        }),
    }
}

/// The change from `current` (`None`: every library) to `desired`
/// (non-empty), or `None` when they match.
fn library_change(current: Option<&[String]>, desired: &[String]) -> Option<Action> {
    let want: BTreeSet<&str> = desired.iter().map(String::as_str).collect();
    let libraries: Vec<String> = want.iter().map(|s| (*s).to_owned()).collect();
    let Some(current) = current else {
        return Some(Action::SetLibraries {
            add: libraries.clone(),
            libraries,
            remove: vec![],
            from_all: true,
        });
    };
    let have: BTreeSet<&str> = current.iter().map(String::as_str).collect();
    if have == want {
        return None;
    }
    Some(Action::SetLibraries {
        libraries,
        add: want.difference(&have).map(|s| (*s).to_owned()).collect(),
        remove: have.difference(&want).map(|s| (*s).to_owned()).collect(),
        from_all: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(id: &str, name: &str) -> Observed {
        Observed {
            remote_id: id.to_owned(),
            username: name.to_owned(),
            enabled: true,
            privileged: false,
            locked_by_service: false,
            libraries: Some(vec!["movies".to_owned(), "tv".to_owned()]),
            gone: false,
            ignored: false,
            locked_by_starr: false,
        }
    }

    fn wanted(id: i64, name: &str) -> Wanted {
        Wanted {
            member_id: id,
            username: name.to_owned(),
            status: MemberStatus::Active,
            libraries: vec!["movies".to_owned(), "tv".to_owned()],
            unmapped: vec![],
            deletion_due: false,
        }
    }

    #[allow(clippy::unnecessary_wraps)] // reads like the planner's input
    fn link(decision: Decision, remote: Option<&str>) -> Option<Link> {
        Some(Link {
            decision,
            remote_id: remote.map(str::to_owned),
            password: PasswordNeed::Nothing,
            expected: None,
            origin: remote.map(|_| Origin::Adopted),
        })
    }

    /// An account Starr created for the member.
    #[allow(clippy::unnecessary_wraps)] // reads like the planner's input
    fn created(remote: &str) -> Option<Link> {
        Some(Link {
            decision: Decision::Create,
            origin: Some(Origin::Provisioned),
            ..link(Decision::Create, Some(remote)).unwrap()
        })
    }

    fn complete(accounts: &[Observed]) -> Instance<'_> {
        Instance {
            listing_complete: true,
            disable: DisableMode::Flag,
            accounts,
        }
    }

    fn step(action: Action, reason: Reason) -> Step {
        Step::new(action, reason)
    }

    fn libs(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    /// Lesson 4: every rule with two members, so one member's state never
    /// leaks into the other's.
    #[test]
    fn undecided_members_get_no_step_only_a_suggestion() {
        let accounts = [account("r-alex", "Alex"), account("r-admin", "sam")];
        let accounts = [
            accounts[0].clone(),
            Observed {
                privileged: true,
                ..accounts[1].clone()
            },
        ];
        let got = plan(
            &complete(&accounts),
            &[(wanted(1, "alex"), None), (wanted(2, "sam"), None)],
        );
        assert_eq!(
            got.entries[0].state,
            State::Undecided {
                suggestion: Some("r-alex".into())
            }
        );
        // A privileged account with the same name is never suggested.
        assert_eq!(got.entries[1].state, State::Undecided { suggestion: None });
        assert!(got.entries.iter().all(|e| e.steps.is_empty()));
        assert_eq!(got.counts().undecided, 2);
        // An explicit "undecided" row behaves the same.
        let got = plan(
            &complete(&accounts),
            &[(wanted(1, "alex"), link(Decision::Undecided, None))],
        );
        assert_eq!(got.entries[0].steps, vec![]);
    }

    #[test]
    fn suggestions_skip_ignored_gone_and_owned_accounts() {
        let accounts = [
            Observed {
                ignored: true,
                ..account("r1", "alex")
            },
            Observed {
                gone: true,
                ..account("r2", "ALEX")
            },
            account("r3", "sam"),
        ];
        let got = plan(
            &complete(&accounts),
            &[
                (wanted(1, "alex"), None),
                (wanted(2, "sam"), None),
                (wanted(3, "mia"), link(Decision::Linked, Some("r3"))),
            ],
        );
        assert_eq!(got.entries[0].state, State::Undecided { suggestion: None });
        // r3 is owned by mia: not offered to sam.
        assert_eq!(got.entries[1].state, State::Undecided { suggestion: None });
    }

    #[test]
    fn create_needs_a_complete_listing_and_a_free_name() {
        let accounts = [account("r-alex", "ALEX")];
        let members = [
            (wanted(1, "alex"), link(Decision::Create, None)),
            (wanted(2, "sam"), link(Decision::Create, None)),
        ];
        let got = plan(&complete(&accounts), &members);
        assert_eq!(
            got.entries[0].state,
            State::Conflict {
                conflict: Conflict::NameTaken {
                    remote_id: "r-alex".into()
                }
            }
        );
        assert_eq!(got.entries[0].steps, vec![]);
        assert_eq!(got.entries[1].state, State::ToCreate);
        assert_eq!(
            got.entries[1].steps,
            vec![step(
                Action::Create {
                    username: "sam".into(),
                    libraries: libs(&["movies", "tv"]),
                },
                Reason::Entitled
            )]
        );
        let c = got.counts();
        assert_eq!((c.create, c.conflicts, c.destructive), (1, 1, 0));

        // Incomplete: nobody is created, whatever the names.
        let incomplete = Instance {
            listing_complete: false,
            disable: DisableMode::Flag,
            accounts: &accounts,
        };
        let got = plan(&incomplete, &members);
        assert!(!got.complete);
        for e in &got.entries {
            assert_eq!(
                e.state,
                State::Waiting {
                    reason: WaitReason::ListingIncomplete
                }
            );
            assert_eq!(e.steps, vec![]);
        }
    }

    #[test]
    fn a_gone_account_frees_its_name() {
        let accounts = [Observed {
            gone: true,
            ..account("old", "sam")
        }];
        let got = plan(
            &complete(&accounts),
            &[(wanted(2, "sam"), link(Decision::Create, None))],
        );
        assert_eq!(got.entries[0].state, State::ToCreate);
    }

    #[test]
    fn nothing_is_created_for_members_who_need_nothing() {
        let suspended = Wanted {
            status: MemberStatus::Suspended,
            ..wanted(1, "alex")
        };
        let no_category = Wanted {
            libraries: vec![],
            ..wanted(2, "sam")
        };
        let got = plan(
            &complete(&[]),
            &[
                (suspended, link(Decision::Create, None)),
                (no_category, link(Decision::Create, None)),
            ],
        );
        for e in &got.entries {
            assert_eq!(e.state, State::NotNeeded);
            assert_eq!(e.steps, vec![]);
        }
    }

    #[test]
    fn no_account_and_ignored_mean_hands_off() {
        let accounts = [
            Observed {
                ignored: true,
                enabled: false,
                ..account("r1", "alex")
            },
            account("r2", "sam"),
        ];
        let got = plan(
            &complete(&accounts),
            &[
                (wanted(1, "alex"), link(Decision::Linked, Some("r1"))),
                (wanted(2, "sam"), link(Decision::None, None)),
            ],
        );
        assert_eq!(
            got.entries[0].state,
            State::HandsOff {
                reason: HandsOffReason::Ignored
            }
        );
        assert_eq!(
            got.entries[1].state,
            State::HandsOff {
                reason: HandsOffReason::NoAccount
            }
        );
        assert!(got.entries.iter().all(|e| e.steps.is_empty()));
    }

    #[test]
    fn privileged_and_missing_owned_accounts_are_conflicts_without_steps() {
        let accounts = [
            Observed {
                privileged: true,
                enabled: false,
                ..account("r1", "alex")
            },
            Observed {
                gone: true,
                enabled: false,
                ..account("r2", "sam")
            },
        ];
        let got = plan(
            &complete(&accounts),
            &[
                (wanted(1, "alex"), link(Decision::Linked, Some("r1"))),
                (wanted(2, "sam"), link(Decision::Create, Some("r2"))),
                (wanted(3, "mia"), link(Decision::Linked, Some("never-seen"))),
            ],
        );
        assert_eq!(
            got.entries[0].state,
            State::Conflict {
                conflict: Conflict::Privileged
            }
        );
        for e in &got.entries[1..] {
            assert_eq!(
                e.state,
                State::Conflict {
                    conflict: Conflict::Missing
                }
            );
        }
        // A missing account is never re-created.
        assert!(got.entries.iter().all(|e| e.steps.is_empty()));
        assert_eq!(got.counts().conflicts, 3);
    }

    #[test]
    fn linked_accounts_get_exactly_their_libraries() {
        let accounts = [
            // Alex sees every library: restricted to the explicit list.
            Observed {
                libraries: None,
                ..account("r1", "Alexander")
            },
            // Sam sees movies and home videos, should see movies + tv.
            Observed {
                libraries: Some(libs(&["movies", "home"])),
                ..account("r2", "sam")
            },
            // Mia is in sync.
            account("r3", "mia"),
        ];
        let got = plan(
            &complete(&accounts),
            &[
                (wanted(1, "alex"), link(Decision::Linked, Some("r1"))),
                (wanted(2, "sam"), link(Decision::Linked, Some("r2"))),
                (wanted(3, "mia"), link(Decision::Linked, Some("r3"))),
            ],
        );
        assert_eq!(
            got.entries[0].steps,
            vec![step(
                Action::SetLibraries {
                    libraries: libs(&["movies", "tv"]),
                    add: libs(&["movies", "tv"]),
                    remove: vec![],
                    from_all: true
                },
                Reason::LibrariesDiffer
            )]
        );
        assert_eq!(
            got.entries[1].steps,
            vec![step(
                Action::SetLibraries {
                    libraries: libs(&["movies", "tv"]),
                    add: libs(&["tv"]),
                    remove: libs(&["home"]),
                    from_all: false
                },
                Reason::LibrariesDiffer
            )]
        );
        assert_eq!(got.entries[2].state, State::Managed);
        assert_eq!(got.entries[2].steps, vec![]);
        assert_eq!(got.entries[0].remote_id.as_deref(), Some("r1"));
        let c = got.counts();
        assert_eq!((c.libraries, c.destructive), (2, 2));
    }

    #[test]
    fn adding_libraries_only_is_not_destructive() {
        let accounts = [Observed {
            libraries: Some(libs(&["movies"])),
            ..account("r1", "alex")
        }];
        let got = plan(
            &complete(&accounts),
            &[(wanted(1, "alex"), link(Decision::Linked, Some("r1")))],
        );
        assert_eq!(got.counts().destructive, 0);
        assert_eq!(got.counts().libraries, 1);
    }

    #[test]
    fn active_members_get_disabled_accounts_back_unless_the_service_locked_them() {
        let accounts = [
            Observed {
                enabled: false,
                ..account("r1", "alex")
            },
            Observed {
                enabled: false,
                locked_by_service: true,
                ..account("r2", "sam")
            },
        ];
        let got = plan(
            &complete(&accounts),
            &[
                (wanted(1, "alex"), link(Decision::Linked, Some("r1"))),
                (wanted(2, "sam"), link(Decision::Create, Some("r2"))),
            ],
        );
        assert_eq!(
            got.entries[0].steps,
            vec![step(Action::Enable, Reason::Entitled)]
        );
        assert_eq!(got.entries[1].state, State::LockedByService);
        assert_eq!(got.entries[1].steps, vec![], "a lockout is never lifted");
    }

    #[test]
    fn members_who_need_nothing_are_disabled_with_libraries_untouched() {
        let accounts = [
            account("r1", "alex"),
            account("r2", "sam"),
            account("r3", "mia"),
            Observed {
                enabled: false,
                ..account("r4", "kim")
            },
        ];
        let members = [
            (
                Wanted {
                    status: MemberStatus::Suspended,
                    ..wanted(1, "alex")
                },
                link(Decision::Linked, Some("r1")),
            ),
            (
                Wanted {
                    status: MemberStatus::Retired,
                    ..wanted(2, "sam")
                },
                link(Decision::Linked, Some("r2")),
            ),
            (
                Wanted {
                    libraries: vec![],
                    ..wanted(3, "mia")
                },
                link(Decision::Create, Some("r3")),
            ),
            (
                Wanted {
                    status: MemberStatus::Suspended,
                    ..wanted(4, "kim")
                },
                link(Decision::Linked, Some("r4")),
            ),
        ];
        let got = plan(&complete(&accounts), &members);
        let reasons: Vec<Vec<Step>> = got.entries.iter().map(|e| e.steps.clone()).collect();
        assert_eq!(
            reasons,
            vec![
                vec![step(Action::Disable, Reason::Suspended)],
                vec![step(Action::Disable, Reason::Retired)],
                vec![step(Action::Disable, Reason::NoCategory)],
                vec![],
            ]
        );
        let c = got.counts();
        assert_eq!((c.disable, c.destructive), (3, 3));
    }

    #[test]
    fn members_wait_while_an_entitled_category_maps_no_library() {
        let accounts = [Observed {
            libraries: None,
            ..account("r1", "alex")
        }];
        let unmapped = |w: Wanted| Wanted {
            unmapped: vec![Category::Series],
            libraries: libs(&["movies"]),
            ..w
        };
        let got = plan(
            &complete(&accounts),
            &[
                (
                    unmapped(wanted(1, "alex")),
                    link(Decision::Linked, Some("r1")),
                ),
                (unmapped(wanted(2, "sam")), link(Decision::Create, None)),
            ],
        );
        for e in &got.entries {
            assert_eq!(
                e.state,
                State::Waiting {
                    reason: WaitReason::LibrariesUnmapped
                }
            );
            assert_eq!(e.steps, vec![]);
        }
        assert_eq!(got.counts().waiting, 2);
    }

    #[test]
    fn suspension_disables_even_while_a_category_is_unmapped() {
        let accounts = [account("r1", "alex"), account("r2", "sam")];
        let unmapped = |w: Wanted| Wanted {
            unmapped: vec![Category::Series],
            ..w
        };
        let got = plan(
            &complete(&accounts),
            &[
                (
                    unmapped(Wanted {
                        status: MemberStatus::Suspended,
                        ..wanted(1, "alex")
                    }),
                    link(Decision::Linked, Some("r1")),
                ),
                (
                    unmapped(wanted(2, "sam")),
                    link(Decision::Linked, Some("r2")),
                ),
                (
                    unmapped(Wanted {
                        status: MemberStatus::Retired,
                        ..wanted(3, "mia")
                    }),
                    link(Decision::Create, None),
                ),
            ],
        );
        assert_eq!(
            got.entries[0].steps,
            vec![step(Action::Disable, Reason::Suspended)]
        );
        assert_eq!(
            got.entries[1].state,
            State::Waiting {
                reason: WaitReason::LibrariesUnmapped
            }
        );
        assert_eq!(got.entries[2].state, State::NotNeeded);
    }

    #[test]
    fn plans_round_trip_through_json() {
        let accounts = [Observed {
            libraries: None,
            ..account("r1", "alex")
        }];
        let got = plan(
            &complete(&accounts),
            &[
                (wanted(1, "alex"), link(Decision::Linked, Some("r1"))),
                (wanted(2, "sam"), link(Decision::Create, None)),
                (wanted(3, "mia"), None),
            ],
        );
        let json = serde_json::to_string(&got).unwrap();
        assert!(json.contains(r#""kind":"setLibraries""#), "{json}");
        assert!(json.contains(r#""fromAll":true"#), "{json}");
        let back: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(back, got);
    }

    fn ts(minutes_ago: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap() - TimeDelta::minutes(minutes_ago)
    }

    /// A complete plan with `disables` disable steps, one enable and one
    /// create (never executable before slice 5).
    fn plan_with(disables: usize, complete: bool) -> Plan {
        let mut entries: Vec<PlanEntry> = (0..disables)
            .map(|i| PlanEntry {
                member_id: i64::try_from(i).unwrap(),
                state: State::Managed,
                remote_id: Some(format!("r{i}")),
                steps: vec![step(Action::Disable, Reason::Suspended)],
            })
            .collect();
        entries.push(PlanEntry {
            member_id: 100,
            state: State::Managed,
            remote_id: Some("r100".into()),
            steps: vec![step(Action::Enable, Reason::Entitled)],
        });
        entries.push(create_entry());
        Plan { complete, entries }
    }

    fn create_entry() -> PlanEntry {
        PlanEntry {
            member_id: 101,
            state: State::ToCreate,
            remote_id: None,
            steps: vec![step(
                Action::Create {
                    username: "new".into(),
                    libraries: libs(&["movies"]),
                },
                Reason::Entitled,
            )],
        }
    }

    #[test]
    fn creates_wait_for_the_password_slice_and_are_not_executable() {
        let p = plan_with(2, true);
        assert_eq!(p.executable_steps(), (3, 2));
        assert_eq!(p.counts().create, 1);
    }

    /// The safe-mode table (M8, D15): one row per rule.
    #[test]
    #[allow(clippy::too_many_lines)] // one table, a row per rule
    fn the_gate_runs_only_live_complete_fresh_plans_within_the_cap() {
        struct Row<'a> {
            name: &'a str,
            mode: WritesMode,
            plan: &'a Plan,
            last_run_minutes_ago: Option<i64>,
            approval: Option<(&'a str, i64)>,
            want: Gate,
        }
        let small = plan_with(2, true);
        let big = plan_with(6, true);
        let at_cap = plan_with(5, true);
        let incomplete = plan_with(1, false);
        let only_create = Plan {
            complete: true,
            entries: vec![create_entry()],
        };
        let empty = Plan {
            complete: true,
            entries: vec![],
        };
        let over = Gate::Hold {
            reason: HoldReason::OverCap {
                destructive: 6,
                cap: DESTRUCTIVE_CAP,
                recent: 0,
            },
        };
        let stale = Gate::Hold {
            reason: HoldReason::NoRecentRun,
        };
        let row = |name, mode, plan, last, approval, want| Row {
            name,
            mode,
            plan,
            last_run_minutes_ago: last,
            approval,
            want,
        };
        let live = WritesMode::Live;
        let rows = [
            row(
                "never live",
                WritesMode::PlanOnly,
                &small,
                Some(1),
                Some(("fp", 1)),
                Gate::PlanOnly,
            ),
            row(
                "paused",
                WritesMode::Paused,
                &small,
                Some(1),
                Some(("fp", 1)),
                Gate::Paused,
            ),
            row(
                "incomplete, even approved",
                live,
                &incomplete,
                Some(1),
                Some(("fp", 1)),
                Gate::Incomplete,
            ),
            row(
                "fresh, within the cap",
                live,
                &small,
                Some(59),
                None,
                Gate::Execute,
            ),
            row(
                "exactly at the cap",
                live,
                &at_cap,
                Some(1),
                None,
                Gate::Execute,
            ),
            row("over the cap", live, &big, Some(1), None, over),
            row(
                "over the cap, approved",
                live,
                &big,
                Some(1),
                Some(("fp", 10)),
                Gate::Execute,
            ),
            row(
                "over the cap, another plan approved",
                live,
                &big,
                Some(1),
                Some(("old", 10)),
                over,
            ),
            row(
                "over the cap, approval too old",
                live,
                &big,
                Some(1),
                Some(("fp", 61)),
                over,
            ),
            row("no live run yet", live, &small, None, None, stale),
            row("last live run too old", live, &small, Some(61), None, stale),
            row(
                "stale, approved",
                live,
                &small,
                None,
                Some(("fp", 0)),
                Gate::Execute,
            ),
            row(
                "stale, nothing to write",
                live,
                &empty,
                None,
                None,
                Gate::Execute,
            ),
            row(
                "stale, only creates waiting",
                live,
                &only_create,
                None,
                None,
                Gate::Execute,
            ),
        ];
        for r in rows {
            let got = gate(&GateInput {
                mode: r.mode,
                plan: r.plan,
                fingerprint: "fp",
                last_live_run: r.last_run_minutes_ago.map(ts),
                approval: r.approval.map(|(fingerprint, ago)| Approval {
                    fingerprint,
                    at: ts(ago),
                }),
                recent_destructive: 0,
                now: ts(0),
            });
            assert_eq!(got, r.want, "{}", r.name);
        }
    }

    /// The rolling window (slice 6): destructive steps already executed on
    /// the instance within the last hour count against the cap, so six
    /// single disables in a row do not pass it one by one.
    #[test]
    fn destructive_steps_of_the_last_hour_count_against_the_cap() {
        let one = plan_with(1, true);
        let input = |plan, recent, approval| GateInput {
            mode: WritesMode::Live,
            plan,
            fingerprint: "fp",
            last_live_run: Some(ts(1)),
            approval,
            recent_destructive: recent,
            now: ts(0),
        };
        assert_eq!(gate(&input(&one, 4, None)), Gate::Execute);
        assert_eq!(
            gate(&input(&one, 5, None)),
            Gate::Hold {
                reason: HoldReason::OverCap {
                    destructive: 1,
                    cap: DESTRUCTIVE_CAP,
                    recent: 5,
                }
            }
        );
        // An approval of exactly this plan lets it run.
        let approval = Some(Approval {
            fingerprint: "fp",
            at: ts(2),
        });
        assert_eq!(gate(&input(&one, 5, approval)), Gate::Execute);
        // A plan without destructive steps is never held by the window.
        let enable_only = Plan {
            complete: true,
            entries: vec![PlanEntry {
                member_id: 1,
                state: State::Managed,
                remote_id: Some("r1".into()),
                steps: vec![step(Action::Enable, Reason::Entitled)],
            }],
        };
        assert_eq!(gate(&input(&enable_only, 9, None)), Gate::Execute);
    }

    #[test]
    fn gates_round_trip_through_json() {
        let g = Gate::Hold {
            reason: HoldReason::OverCap {
                destructive: 6,
                cap: 5,
                recent: 2,
            },
        };
        let json = serde_json::to_string(&g).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"hold","reason":{"kind":"overCap","destructive":6,"cap":5,"recent":2}}"#
        );
        assert_eq!(serde_json::from_str::<Gate>(&json).unwrap(), g);
        // Written before the window existed.
        let old = r#"{"kind":"hold","reason":{"kind":"overCap","destructive":6,"cap":5}}"#;
        assert!(serde_json::from_str::<Gate>(old).is_ok());
    }

    #[test]
    fn plans_stored_before_the_full_library_list_still_parse() {
        let old = r#"{"kind":"setLibraries","add":["a"],"remove":[],"fromAll":true}"#;
        let action: Action = serde_json::from_str(old).unwrap();
        assert!(matches!(action, Action::SetLibraries { libraries, .. } if libraries.is_empty()));
    }

    #[test]
    fn names_round_trip() {
        for d in Decision::ALL {
            assert_eq!(Decision::parse(d.as_str()), Some(d));
        }
        for o in [Origin::Provisioned, Origin::Adopted] {
            assert_eq!(Origin::parse(o.as_str()), Some(o));
        }
        for p in [
            PasswordUse::Starr,
            PasswordUse::Deferred,
            PasswordUse::Pending,
        ] {
            assert_eq!(PasswordUse::parse(p.as_str()), Some(p));
        }
        assert_eq!(Decision::parse("later"), None);
    }

    #[test]
    fn a_password_is_due_only_when_its_generation_differs_or_a_switch_waits() {
        use PasswordUse::{Deferred, Pending, Starr};
        assert_eq!(
            PasswordNeed::of(Some(Starr), Some(3), 3),
            PasswordNeed::Nothing
        );
        assert_eq!(
            PasswordNeed::of(Some(Starr), Some(2), 3),
            PasswordNeed::Changed
        );
        assert_eq!(
            PasswordNeed::of(Some(Starr), None, 3),
            PasswordNeed::Missing
        );
        assert_eq!(
            PasswordNeed::of(Some(Deferred), None, 3),
            PasswordNeed::Nothing
        );
        assert_eq!(
            PasswordNeed::of(Some(Pending), None, 3),
            PasswordNeed::Switch
        );
        assert_eq!(PasswordNeed::of(None, None, 3), PasswordNeed::Nothing);
    }

    #[allow(clippy::unnecessary_wraps)] // reads like the planner's input
    fn linked_with(remote: &str, password: PasswordNeed) -> Option<Link> {
        Some(Link {
            decision: Decision::Linked,
            remote_id: Some(remote.to_owned()),
            password,
            expected: None,
            origin: Some(Origin::Adopted),
        })
    }

    #[allow(clippy::unnecessary_wraps)] // reads like the planner's input
    fn expected(enabled: bool, libraries: &[&str]) -> Option<Expected> {
        Some(Expected {
            enabled,
            libraries: Some(libs(libraries)),
        })
    }

    fn retired_due(id: i64, name: &str) -> Wanted {
        Wanted {
            status: MemberStatus::Retired,
            deletion_due: true,
            libraries: vec![],
            ..wanted(id, name)
        }
    }

    /// D16 with two members: only the member whose deletion order is due
    /// loses the account; a retired member within the grace period is only
    /// disabled. Privileged, ignored and hands-off accounts get no step.
    #[test]
    fn accounts_are_deleted_only_when_the_deletion_order_is_due() {
        let accounts = [
            Observed {
                enabled: false,
                ..account("r1", "alex")
            },
            account("r2", "sam"),
            Observed {
                privileged: true,
                ..account("r3", "mia")
            },
            Observed {
                ignored: true,
                ..account("r4", "kim")
            },
        ];
        let in_grace = Wanted {
            status: MemberStatus::Retired,
            libraries: vec![],
            ..wanted(2, "sam")
        };
        let got = plan(
            &complete(&accounts),
            &[
                (retired_due(1, "alex"), created("r1")),
                (in_grace, link(Decision::Linked, Some("r2"))),
                (retired_due(3, "mia"), created("r3")),
                (retired_due(4, "kim"), created("r4")),
                (retired_due(5, "lou"), link(Decision::None, None)),
            ],
        );
        assert_eq!(
            got.entries[0].steps,
            vec![step(Action::Delete, Reason::DeletionDue)]
        );
        assert_eq!(
            got.entries[1].steps,
            vec![step(Action::Disable, Reason::Retired)]
        );
        assert!(got.entries[2..].iter().all(|e| e.steps.is_empty()));
        let c = got.counts();
        assert_eq!((c.delete, c.destructive), (1, 2));
        assert_eq!(got.executable_steps(), (2, 2));
    }

    /// D16 as decided by the maintainer (2026-11-03), two members: the
    /// account Starr created is deleted; the adopted one is only disabled,
    /// and once disabled gets no step at all (Starr releases it).
    #[test]
    fn only_accounts_starr_created_are_deleted_adopted_ones_are_disabled() {
        let accounts = [
            account("r1", "alex"),
            account("r2", "sam"),
            Observed {
                enabled: false,
                ..account("r3", "mia")
            },
        ];
        let got = plan(
            &complete(&accounts),
            &[
                (retired_due(1, "alex"), created("r1")),
                (retired_due(2, "sam"), link(Decision::Linked, Some("r2"))),
                (retired_due(3, "mia"), link(Decision::Linked, Some("r3"))),
            ],
        );
        assert_eq!(
            got.entries[0].steps,
            vec![step(Action::Delete, Reason::DeletionDue)]
        );
        assert_eq!(
            got.entries[1].steps,
            vec![step(Action::Disable, Reason::DeletionDue)]
        );
        assert_eq!(got.entries[2].steps, vec![]);
        assert_eq!(delete_plan(&got.entries[1], &got.entries[1].steps[0]), None);
        assert_eq!(got.counts().delete, 1);
    }

    /// The only way to a `DeletePlan`: a `Delete` step of a due deletion
    /// that the entry itself carries, for the account it owns.
    #[test]
    fn a_delete_plan_exists_only_for_a_planned_due_deletion() {
        let accounts = [account("r1", "alex"), account("r2", "sam")];
        let got = plan(
            &complete(&accounts),
            &[
                (retired_due(1, "alex"), created("r1")),
                (
                    Wanted {
                        status: MemberStatus::Suspended,
                        ..wanted(2, "sam")
                    },
                    link(Decision::Linked, Some("r2")),
                ),
            ],
        );
        let due = &got.entries[0];
        let plan = delete_plan(due, &due.steps[0]).unwrap();
        assert_eq!(plan.remote_id(), "r1");
        assert!(plan.check("r1").is_ok());
        assert!(plan.check("r2").is_err());
        // A step the entry does not carry, or of another kind, gives none.
        let other = &got.entries[1];
        assert_eq!(delete_plan(other, &other.steps[0]), None);
        let foreign = step(Action::Delete, Reason::DeletionDue);
        assert_eq!(delete_plan(other, &foreign), None);
        let wrong_reason = PlanEntry {
            steps: vec![step(Action::Delete, Reason::Suspended)],
            ..other.clone()
        };
        assert_eq!(delete_plan(&wrong_reason, &wrong_reason.steps[0]), None);
    }

    /// D13 with two members: a change made in the service since Starr's
    /// last write is undone and marked as a deviation; the same step
    /// without a baseline (never written) is not.
    #[test]
    fn changes_made_in_the_service_are_undone_as_deviations() {
        let accounts = [
            Observed {
                enabled: false,
                libraries: Some(libs(&["movies"])),
                ..account("r1", "alex")
            },
            Observed {
                enabled: false,
                libraries: Some(libs(&["movies"])),
                ..account("r2", "sam")
            },
            account("r3", "mia"),
        ];
        let with = |remote: &str, expected: Option<Expected>| {
            Some(Link {
                expected,
                ..link(Decision::Linked, Some(remote)).unwrap()
            })
        };
        let got = plan(
            &complete(&accounts),
            &[
                (
                    wanted(1, "alex"),
                    with("r1", expected(true, &["movies", "tv"])),
                ),
                (wanted(2, "sam"), with("r2", None)),
                (
                    Wanted {
                        status: MemberStatus::Suspended,
                        ..wanted(3, "mia")
                    },
                    with("r3", expected(false, &["movies", "tv"])),
                ),
            ],
        );
        assert!(got.entries[0].steps.iter().all(|s| s.drift));
        assert_eq!(got.entries[0].steps.len(), 2);
        assert!(got.entries[1].steps.iter().all(|s| !s.drift));
        assert_eq!(got.entries[1].steps.len(), 2);
        // Enabled again in the service while suspended.
        assert_eq!(
            got.entries[2].steps,
            vec![Step {
                drift: true,
                ..step(Action::Disable, Reason::Suspended)
            }]
        );
        assert_eq!(got.counts().drift, 3);
        // Drift round-trips; a step without it serializes as before.
        let json = serde_json::to_string(&got.entries[2].steps[0]).unwrap();
        assert!(json.contains(r#""drift":true"#), "{json}");
        let plain = serde_json::to_string(&got.entries[1].steps[0]).unwrap();
        assert!(!plain.contains("drift"), "{plain}");
    }

    #[test]
    fn password_writes_are_planned_for_active_members_only() {
        let accounts = [account("r1", "alex"), account("r2", "sam")];
        let suspended = Wanted {
            status: MemberStatus::Suspended,
            ..wanted(2, "sam")
        };
        let got = plan(
            &complete(&accounts),
            &[
                (wanted(1, "alex"), linked_with("r1", PasswordNeed::Changed)),
                (suspended, linked_with("r2", PasswordNeed::Changed)),
            ],
        );
        assert_eq!(
            got.entries[0].steps,
            vec![step(Action::SetPassword, Reason::PasswordChanged)]
        );
        // D9: never a password for a suspended member, only the disable.
        assert_eq!(
            got.entries[1].steps,
            vec![step(Action::Disable, Reason::Suspended)]
        );
        assert_eq!(got.counts().password, 1);
        // A password write needs the member's password: never run by a
        // plan run, never destructive, never counted against the cap.
        assert_eq!(got.executable_steps(), (1, 1));
        assert!(!step(Action::SetPassword, Reason::PasswordChanged).destructive());

        let got = plan(
            &complete(&accounts),
            &[(wanted(1, "alex"), linked_with("r1", PasswordNeed::Switch))],
        );
        assert_eq!(
            got.entries[0].steps,
            vec![step(Action::SetPassword, Reason::PasswordSwitch)]
        );
    }

    #[test]
    fn an_account_without_password_is_disabled_and_never_enabled() {
        let movies = libs(&["movies", "tv"]);
        let enabled = Observed {
            libraries: Some(movies.clone()),
            ..account("r1", "alex")
        };
        let disabled = Observed {
            enabled: false,
            libraries: Some(movies),
            ..account("r2", "sam")
        };
        let accounts = [enabled, disabled];
        let got = plan(
            &complete(&accounts),
            &[
                (wanted(1, "alex"), linked_with("r1", PasswordNeed::Missing)),
                (wanted(2, "sam"), linked_with("r2", PasswordNeed::Missing)),
            ],
        );
        assert_eq!(
            got.entries[0].steps,
            vec![
                step(Action::Disable, Reason::PasswordMissing),
                step(Action::SetPassword, Reason::PasswordMissing),
            ]
        );
        assert_eq!(
            got.entries[1].steps,
            vec![step(Action::SetPassword, Reason::PasswordMissing)],
            "no enable before a password is written"
        );
    }

    fn entry(state: State, remote: Option<&str>, steps: Vec<Step>) -> PlanEntry {
        PlanEntry {
            member_id: 1,
            state,
            remote_id: remote.map(str::to_owned),
            steps,
        }
    }

    /// M9: one row per member-facing rule.
    #[test]
    #[allow(clippy::type_complexity)] // one table, a row per rule
    fn members_see_their_services_in_member_words() {
        use MemberStatus::{Active, Retired, Suspended as Susp};
        use MemberView::{Activate, OldPassword, Ready, SettingUp, Suspended};
        use PasswordUse::{Deferred, Pending, Starr};
        let pw = || step(Action::SetPassword, Reason::PasswordChanged);
        let create = || {
            step(
                Action::Create {
                    username: "alex".into(),
                    libraries: libs(&["movies"]),
                },
                Reason::Entitled,
            )
        };
        let managed = |steps| entry(State::Managed, Some("r1"), steps);
        let to_create = entry(State::ToCreate, None, vec![create()]);
        let in_sync = managed(vec![]);
        let due = managed(vec![pw()]);
        let enabling = managed(vec![step(Action::Enable, Reason::Entitled)]);
        let waiting = entry(
            State::Waiting {
                reason: WaitReason::LibrariesUnmapped,
            },
            None,
            vec![],
        );
        let undecided = entry(State::Undecided { suggestion: None }, None, vec![]);
        let not_needed = entry(State::NotNeeded, None, vec![]);
        #[rustfmt::skip]
        let rows: [(&str, MemberStatus, &PlanEntry, Option<PasswordUse>, bool, bool, Option<MemberView>); 15] = [
            ("in sync", Active, &in_sync, Some(Starr), true, false, Some(Ready)),
            ("in sync, plan only", Active, &in_sync, Some(Starr), false, false, Some(Ready)),
            ("being enabled", Active, &enabling, Some(Starr), true, false, Some(SettingUp)),
            ("password due, live", Active, &due, Some(Starr), true, false, Some(Activate)),
            ("password due, handoff in flight", Active, &due, Some(Starr), true, true, Some(SettingUp)),
            ("password due, not live", Active, &due, Some(Starr), false, false, Some(SettingUp)),
            ("to create, live", Active, &to_create, None, true, false, Some(Activate)),
            ("to create, handoff", Active, &to_create, None, true, true, Some(SettingUp)),
            ("adopted", Active, &in_sync, Some(Deferred), true, false, Some(OldPassword)),
            ("switch in flight", Active, &due, Some(Pending), true, true, Some(SettingUp)),
            ("switch expired", Active, &due, Some(Pending), true, false, Some(OldPassword)),
            ("waiting for the admin", Active, &waiting, None, true, false, Some(SettingUp)),
            ("undecided: not shown", Active, &undecided, None, true, false, None),
            ("suspended with an account", Susp, &in_sync, Some(Starr), true, false, Some(Suspended)),
            ("suspended without an account", Susp, &not_needed, None, true, false, None),
        ];
        for (name, status, entry, password, live, handoff, want) in rows {
            let got = member_view(&MemberViewInput {
                status,
                entry,
                password,
                live,
                handoff,
            });
            assert_eq!(got, want, "{name}");
        }
        let retired = member_view(&MemberViewInput {
            status: Retired,
            entry: &in_sync,
            password: Some(Starr),
            live: true,
            handoff: false,
        });
        assert_eq!(retired, None);
    }

    fn scrambling(accounts: &[Observed]) -> Instance<'_> {
        Instance {
            disable: DisableMode::ScramblePassword,
            ..complete(accounts)
        }
    }

    /// A linked account whose password needs `password`.
    #[allow(clippy::unnecessary_wraps)] // reads like the planner's input
    fn locked(remote: &str, password: PasswordNeed) -> Option<Link> {
        Some(Link {
            password,
            ..link(Decision::Linked, Some(remote)).unwrap()
        })
    }

    /// An account Starr locked with a random password.
    fn lock(account: Observed) -> Observed {
        Observed {
            locked_by_starr: true,
            ..account
        }
    }

    /// D9 with two members: on a service without a disable flag a
    /// suspension scrambles the password (a `Disable` step), and an
    /// account Starr locked that way is never "enabled": the active
    /// member's account waits for their password, with no enable step.
    #[test]
    fn without_a_disable_flag_disable_scrambles_and_only_a_password_unlocks() {
        let accounts = [account("r1", "alex"), lock(account("r2", "sam"))];
        let mut suspended = wanted(1, "alex");
        suspended.status = MemberStatus::Suspended;
        let got = plan(
            &scrambling(&accounts),
            &[
                (suspended, link(Decision::Linked, Some("r1"))),
                (wanted(2, "sam"), locked("r2", PasswordNeed::Missing)),
            ],
        );
        assert_eq!(
            got.entries[0].steps,
            vec![step(Action::Disable, Reason::Suspended)]
        );
        assert_eq!(
            got.entries[1].steps,
            vec![step(Action::SetPassword, Reason::LockedByStarr)]
        );
        assert_eq!(got.entries[1].state, State::Managed);
        let counts = got.counts();
        assert_eq!((counts.enable, counts.disable, counts.password), (0, 1, 1));
        // Only the scramble runs in a plan run; the unlock waits for the
        // member's password.
        assert_eq!(got.executable_steps(), (1, 1));
    }

    /// The same records with a disable flag: Starr's lock is not a thing
    /// there, the service's flag decides (the suspension disables, the
    /// active member's disabled account is enabled).
    #[test]
    fn with_a_disable_flag_the_service_flag_decides() {
        let accounts = [
            account("r1", "alex"),
            Observed {
                enabled: false,
                ..account("r2", "sam")
            },
        ];
        let mut suspended = wanted(1, "alex");
        suspended.status = MemberStatus::Suspended;
        let got = plan(
            &complete(&accounts),
            &[
                (suspended, link(Decision::Linked, Some("r1"))),
                (wanted(2, "sam"), link(Decision::Linked, Some("r2"))),
            ],
        );
        assert_eq!(
            got.entries[0].steps,
            vec![step(Action::Disable, Reason::Suspended)]
        );
        assert_eq!(
            got.entries[1].steps,
            vec![step(Action::Enable, Reason::Entitled)]
        );
    }

    /// A locked account is never scrambled again (nothing to disable), for
    /// a suspended member nothing is written (D9: never a password), and
    /// the service's "enabled" (always true without a flag) is ignored.
    #[test]
    fn a_locked_account_gets_no_second_scramble_and_no_password_while_suspended() {
        let accounts = [
            lock(account("r1", "alex")),
            Observed {
                // Defensive: a service without a flag never says this.
                enabled: false,
                ..account("r2", "sam")
            },
        ];
        let mut suspended = wanted(1, "alex");
        suspended.status = MemberStatus::Suspended;
        let got = plan(
            &scrambling(&accounts),
            &[
                (suspended, locked("r1", PasswordNeed::Missing)),
                (wanted(2, "sam"), link(Decision::Linked, Some("r2"))),
            ],
        );
        assert_eq!(got.entries[0].steps, vec![]);
        assert_eq!(got.entries[1].steps, vec![], "nothing to enable");
    }

    /// Libraries are set before the unlock (the account sees only its
    /// libraries once the member logs in); a pending password change or
    /// switch becomes the one unlocking password step.
    #[test]
    fn libraries_come_before_the_unlocking_password() {
        let accounts = [
            lock(Observed {
                libraries: Some(libs(&["movies"])),
                ..account("r1", "alex")
            }),
            lock(account("r2", "sam")),
        ];
        let got = plan(
            &scrambling(&accounts),
            &[
                (wanted(1, "alex"), locked("r1", PasswordNeed::Missing)),
                (wanted(2, "sam"), locked("r2", PasswordNeed::Changed)),
            ],
        );
        let actions: Vec<_> = got.entries[0].steps.iter().map(|s| &s.action).collect();
        assert!(matches!(
            actions[..],
            [Action::SetLibraries { .. }, Action::SetPassword]
        ));
        assert_eq!(got.entries[0].steps[1].reason, Reason::LockedByStarr);
        assert_eq!(
            got.entries[1].steps,
            vec![step(Action::SetPassword, Reason::LockedByStarr)]
        );
    }

    /// D16 without a flag: an adopted account at the deadline is scrambled
    /// (then released), once; one Starr created is deleted, locked or not.
    #[test]
    fn deletion_due_without_a_flag_scrambles_adopted_accounts_once() {
        let accounts = [
            account("r1", "alex"),
            lock(account("r2", "sam")),
            lock(account("r3", "mia")),
        ];
        let due = |id, name| Wanted {
            status: MemberStatus::Retired,
            deletion_due: true,
            libraries: vec![],
            ..wanted(id, name)
        };
        let got = plan(
            &scrambling(&accounts),
            &[
                (due(1, "alex"), link(Decision::Linked, Some("r1"))),
                (due(2, "sam"), locked("r2", PasswordNeed::Missing)),
                (due(3, "mia"), created("r3")),
            ],
        );
        assert_eq!(
            got.entries[0].steps,
            vec![step(Action::Disable, Reason::DeletionDue)]
        );
        assert_eq!(got.entries[1].steps, vec![]);
        assert_eq!(
            got.entries[2].steps,
            vec![step(Action::Delete, Reason::DeletionDue)]
        );
    }

    /// My services: a locked account asks for the member's password
    /// ("activate"), also when it was adopted with the old password; with
    /// a handoff on its way or writes off it is "being set up"; suspended
    /// stays suspended.
    #[test]
    fn a_locked_account_asks_the_member_to_activate() {
        let entry = PlanEntry {
            member_id: 1,
            state: State::Managed,
            remote_id: Some("r1".into()),
            steps: vec![step(Action::SetPassword, Reason::LockedByStarr)],
        };
        let view = |status, password, live, handoff| {
            member_view(&MemberViewInput {
                status,
                entry: &entry,
                password,
                live,
                handoff,
            })
        };
        let active = MemberStatus::Active;
        assert_eq!(
            view(active, Some(PasswordUse::Starr), true, false),
            Some(MemberView::Activate)
        );
        assert_eq!(
            view(active, Some(PasswordUse::Deferred), true, false),
            Some(MemberView::Activate)
        );
        assert_eq!(
            view(active, Some(PasswordUse::Starr), true, true),
            Some(MemberView::SettingUp)
        );
        assert_eq!(
            view(active, Some(PasswordUse::Starr), false, false),
            Some(MemberView::SettingUp)
        );
        assert_eq!(
            view(
                MemberStatus::Suspended,
                Some(PasswordUse::Starr),
                true,
                false
            ),
            Some(MemberView::Suspended)
        );
    }

    /// Without a flag nothing shows a disable undone in the service: an
    /// unlocked account Starr once scrambled is disabled again without a
    /// deviation; with a flag the same baseline is one.
    #[test]
    fn without_a_flag_a_disable_is_never_a_deviation() {
        let accounts = [account("r1", "alex"), account("r2", "sam")];
        let baseline = |remote| {
            Some(Link {
                expected: Some(Expected {
                    enabled: false,
                    libraries: None,
                }),
                ..link(Decision::Linked, Some(remote)).unwrap()
            })
        };
        let suspended = |id, name| Wanted {
            status: MemberStatus::Suspended,
            ..wanted(id, name)
        };
        let members = [
            (suspended(1, "alex"), baseline("r1")),
            (suspended(2, "sam"), link(Decision::Linked, Some("r2"))),
        ];
        let without = plan(&scrambling(&accounts), &members);
        assert!(without.entries.iter().all(|e| !e.steps[0].drift));
        let with = plan(&complete(&accounts), &members);
        assert!(with.entries[0].steps[0].drift);
        assert!(!with.entries[1].steps[0].drift);
    }
}
