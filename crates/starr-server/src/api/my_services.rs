//! "My services" (spec 0005 slice 5, M9, ADR 0010), members only:
//! - `GET /api/v1/me/services`: the consume instances where the member has
//!   or should have an account, in member words (ready · activate with
//!   your password · uses your old password · being set up · suspended),
//!   with the username there when it differs and the server address for
//!   apps (the public URL, never the base URL). Never remote IDs, errors,
//!   libraries or other people;
//! - `POST /api/v1/me/services/activate`: the member enters their Starr
//!   password so Starr can create or update the accounts that wait for it;
//! - `POST /api/v1/me/services/{serviceId}/use-starr-password`: switch an
//!   adopted account to the Starr password (M2), which signs out the
//!   account's devices.
//!
//! Both writes need the current Starr password, verified against the hash
//! and throttled like a login (M10 3), so a stolen session cannot set a
//! member's service passwords. The password goes to the worker only as a
//! sealed handoff ([`offer_password`]); it is never logged, stored in plain
//! text, audited or echoed.

use axum::Json;
use axum::extract::{Path, State};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::categories::is_private;
use starr_core::libraries;
use starr_core::members::{Category, Entitlement, MemberStatus};
use starr_core::provisioning::{
    Conflict, Decision, MemberView, MemberViewInput, PasswordUse, Plan, PlanEntry,
    State as PlanState, WritesMode, member_view,
};
use starr_db::service_accounts::SwitchOutcome;
use starr_worker::provisioning::{account_instances, enqueue_password};
use starr_worker::secrets::seal_password_handoff;
use utoipa::ToSchema;

use super::common;
use super::error::{ApiError, ErrorBody};
use crate::AppState;
use crate::auth::{Client, MemberSession};

/// What became of a password offered for a handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Offer {
    /// Sealed and stored; `accounts.password` is queued.
    Stored,
    /// Nothing could use it now (no account waits for it on an instance
    /// with account writes switched on): not stored at all.
    NotNeeded,
    /// The worker has not published its key yet: not stored.
    Unavailable,
}

/// Offer the member's password at `generation` to the worker (ADR 0010):
/// sealed to the worker's recipient, bound to the member and generation,
/// valid for an hour, only when an account can use it. The plaintext stays
/// in this process's memory.
pub(crate) async fn offer_password(
    state: &AppState,
    member_id: i64,
    generation: i64,
    password: &SecretString,
) -> Result<Offer, ApiError> {
    if !state.db.password_handoff_wanted(member_id).await? {
        return Ok(Offer::NotNeeded);
    }
    let sealer = match state.credential_sealer().await {
        Ok(sealer) => sealer,
        Err(ApiError::Unavailable(_)) => return Ok(Offer::Unavailable),
        Err(err) => return Err(err),
    };
    let (recipient, ciphertext) = seal_password_handoff(&sealer, member_id, generation, password)
        .map_err(|err| {
        tracing::error!(error = %err, "sealing a password handoff failed");
        ApiError::Internal
    })?;
    let stored = state
        .db
        .store_password_handoff(
            member_id,
            generation,
            &recipient,
            &ciphertext,
            state.clock.now(),
        )
        .await?;
    if !stored {
        // Suspended or a newer password meanwhile.
        return Ok(Offer::NotNeeded);
    }
    enqueue_password(&state.db).await?;
    state.wake.notify_one();
    Ok(Offer::Stored)
}

/// [`offer_password`] after a password was set (invitation, set-password
/// link, password change): best effort, the password is set either way and
/// accounts that miss it show "activate".
pub(crate) async fn offer_after_set(
    state: &AppState,
    member_id: i64,
    generation: i64,
    password: &SecretString,
) {
    match offer_password(state, member_id, generation, password).await {
        Ok(Offer::Unavailable) => tracing::warn!(
            member = member_id,
            "no worker key yet: the new password reaches the services when the member activates"
        ),
        Ok(_) => {}
        Err(err) => tracing::error!(
            member = member_id,
            code = err.code(),
            "the new password could not be handed to the worker; the member can activate later"
        ),
    }
}

/// One service in member words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = MyServiceState)]
pub enum MyServiceStateDto {
    /// Works with the Starr password.
    Ready,
    /// Starr needs the member's password once ("activate").
    Activate,
    /// Adopted: still the old password; "use my Starr password here".
    OldPassword,
    /// Starr is working on it right now.
    SettingUp,
    /// An admin has to act first; `waitingFor` says what for.
    Waiting,
    /// The member has a category there, but no admin decided about an
    /// account yet.
    Undecided,
    /// The member is suspended.
    Suspended,
}

/// What a waiting service waits for, in member words (MEM-22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = MyServiceWaitingFor)]
pub enum WaitingForDto {
    /// The member's name is taken there; the admin links or renames.
    NameTaken,
    /// The service locked the account after failed sign-ins.
    Locked,
    /// Starr does not write to the service yet (plan only or paused).
    WritesOff,
    /// Something else only an admin can settle (libraries not mapped, an
    /// incomplete account listing, a privileged or missing account).
    Admin,
}

/// The member's state on one instance with what it waits for: "being set
/// up" from [`member_view`] splits into Starr at work and an admin to act
/// (MEM-22), so a taken name never reads as "a moment".
fn shown_state(
    view: MemberView,
    entry: Option<&PlanEntry>,
    live: bool,
    handoff: bool,
) -> (MyServiceStateDto, Option<WaitingForDto>) {
    let waiting = |reason| (MyServiceStateDto::Waiting, Some(reason));
    match view {
        MemberView::Ready => (MyServiceStateDto::Ready, None),
        MemberView::Activate => (MyServiceStateDto::Activate, None),
        MemberView::OldPassword => (MyServiceStateDto::OldPassword, None),
        MemberView::Suspended => (MyServiceStateDto::Suspended, None),
        MemberView::SettingUp => match entry.map(|e| &e.state) {
            Some(PlanState::Conflict {
                conflict: Conflict::NameTaken { .. },
            }) => waiting(WaitingForDto::NameTaken),
            Some(PlanState::LockedByService) => waiting(WaitingForDto::Locked),
            Some(PlanState::Conflict { .. } | PlanState::Waiting { .. }) => {
                waiting(WaitingForDto::Admin)
            }
            // A handoff is on its way: the worker writes it shortly.
            _ if handoff => (MyServiceStateDto::SettingUp, None),
            _ if !live => waiting(WaitingForDto::WritesOff),
            _ => (MyServiceStateDto::SettingUp, None),
        },
    }
}

/// One consume service of the member.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)] // independent facts shown to the member
pub struct MyService {
    service_id: i64,
    name: String,
    state: MyServiceStateDto,
    /// With `waiting`: what an admin has to do first.
    waiting_for: Option<WaitingForDto>,
    /// The username there, only when it differs from the member's.
    username: Option<String>,
    /// "Server address" for apps: the service's public URL; absent when
    /// the admin set none.
    server_address: Option<String>,
    /// The account uses the Starr password (a password change applies
    /// here).
    uses_starr_password: bool,
    /// Writing a password there signs out every device of the account.
    password_change_signs_out: bool,
    /// Starr writes to this service now (account writes live): "activate"
    /// and "use my Starr password here" take effect right away.
    accepts_password: bool,
    /// Starr locked the account with a random password (the service has no
    /// off switch, Navidrome): the member's next password unlocks it
    /// ("activate" once reactivated).
    locked_by_starr: bool,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyServices {
    services: Vec<MyService>,
}

/// The member may use one of `categories` (an instance's consume roles)
/// and it is not the private one: only then is an undecided instance
/// listed, so no service ever hints at the private category.
fn has_public_category(
    mut categories: impl Iterator<Item = Category>,
    entitlements: &[Entitlement],
    enabled: &[Category],
) -> bool {
    categories.any(|c| !is_private(c) && libraries::entitled(entitlements, enabled, c))
}

async fn my_services(state: &AppState, member_id: i64) -> Result<MyServices, ApiError> {
    let member = state
        .db
        .get_member(member_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    let now = state.clock.now();
    let handoff = state.db.has_password_handoff(member_id, now).await?;
    let rows = state.db.member_service_accounts(member_id).await?;
    let entitlements = state.db.entitlements(member_id).await?;
    let enabled: Vec<Category> = state
        .db
        .list_categories()
        .await?
        .into_iter()
        .filter(|c| c.enabled)
        .map(|c| c.category)
        .collect();
    let mut services = Vec::new();
    for (id, name, roles) in account_instances(&state.db).await? {
        let Some(stored) = state.db.account_plan(id).await? else {
            continue;
        };
        let Ok(plan) = serde_json::from_str::<Plan>(&stored.plan) else {
            continue;
        };
        let row = rows.iter().find(|r| r.service_instance_id == id);
        let password = row.and_then(|r| r.password);
        let live = state
            .db
            .account_writes(id)
            .await?
            .is_some_and(|w| w.mode() == WritesMode::Live);
        let entry = plan.entries.iter().find(|e| e.member_id == member_id);
        let view = match entry {
            Some(entry) => member_view(&MemberViewInput {
                status: member.status,
                entry,
                password,
                live,
                handoff,
            }),
            // Decided, but not planned yet (a new member a moment ago).
            None => row
                .filter(|r| {
                    member.status == MemberStatus::Active
                        && matches!(r.decision, Decision::Create | Decision::Linked)
                })
                .map(|_| MemberView::SettingUp),
        };
        let shown = match view {
            Some(view) => shown_state(view, entry, live, handoff),
            // Nobody decided yet although the member has a category there:
            // listed so the member knows it is coming (MEM-22). The private
            // category never counts, so no service hints at it.
            None if entry.is_some_and(|e| matches!(e.state, PlanState::Undecided { .. }))
                && member.status == MemberStatus::Active
                && has_public_category(
                    roles.iter().map(|(category, _)| *category),
                    &entitlements,
                    &enabled,
                ) =>
            {
                (MyServiceStateDto::Undecided, None)
            }
            None => continue,
        };
        let Some(service) = state.db.get_service(id).await? else {
            continue;
        };
        let username = row
            .and_then(|r| r.linked_username.clone())
            .filter(|there| !there.eq_ignore_ascii_case(&member.username));
        let rules = starr_worker::adapters::account_rules(&service.adapter_type);
        let locked_by_starr = match row.and_then(|r| r.remote_id.as_deref()) {
            Some(remote_id) => state
                .db
                .remote_accounts(id)
                .await?
                .iter()
                .any(|a| a.remote_id == remote_id && a.locked_by_starr),
            None => false,
        };
        services.push(MyService {
            service_id: id,
            name,
            state: shown.0,
            waiting_for: shown.1,
            username,
            server_address: service.public_url,
            uses_starr_password: password == Some(PasswordUse::Starr),
            password_change_signs_out: rules.is_some_and(|r| r.password_change_signs_out),
            accepts_password: live,
            locked_by_starr,
        });
    }
    Ok(MyServices { services })
}

/// The member's consume services in member words (M9).
#[utoipa::path(get, path = "/api/v1/me/services", tag = "me",
    responses(
        (status = 200, body = MyServices),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not a member)", body = ErrorBody),
    ))]
pub async fn get_my_services(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
) -> Result<Json<MyServices>, ApiError> {
    Ok(Json(my_services(&state, session.subject.id).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfirmPasswordRequest {
    /// The member's current Starr password.
    #[schema(value_type = String, format = Password)]
    password: SecretString,
}

/// The member's password, checked against the hash and throttled like a
/// login (M10 3).
async fn checked_password(
    state: &AppState,
    client: &Client,
    session: &crate::auth::Session,
    password: SecretString,
) -> Result<SecretString, ApiError> {
    let hash = state.db.member_password_hash(session.subject.id).await?;
    common::check_password(state, client, &session.subject.username, hash, password)
        .await?
        .ok_or(ApiError::Validation("auth.current_password_invalid"))
}

async fn offered(
    state: &AppState,
    member_id: i64,
    password: &SecretString,
) -> Result<(), ApiError> {
    let generation = state
        .db
        .member_password_generation(member_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    match offer_password(state, member_id, generation, password).await? {
        Offer::Stored => Ok(()),
        Offer::NotNeeded => Err(ApiError::Conflict("services.nothing_to_activate")),
        Offer::Unavailable => Err(ApiError::Unavailable("services.secrets_unavailable")),
    }
}

/// Activate: hand the member's Starr password to Starr once, so it can
/// create or update the accounts that wait for it (ADR 0010). Needs the
/// current password; wrong ones count against the login throttle.
#[utoipa::path(post, path = "/api/v1/me/services/activate", tag = "me",
    request_body = ConfirmPasswordRequest,
    responses(
        (status = 200, description = "Handed over; the accounts are being set up", body = MyServices),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not a member)", body = ErrorBody),
        (status = 409, description = "`services.nothing_to_activate`", body = ErrorBody),
        (status = 422, description = "`auth.current_password_invalid`", body = ErrorBody),
        (status = 429, description = "`auth.too_many_attempts`", body = ErrorBody),
        (status = 503, description = "`services.secrets_unavailable` (the worker is not running yet)", body = ErrorBody),
    ))]
pub async fn activate(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    client: Client,
    Json(body): Json<ConfirmPasswordRequest>,
) -> Result<Json<MyServices>, ApiError> {
    let password = checked_password(&state, &client, &session, body.password).await?;
    offered(&state, session.subject.id, &password).await?;
    drop(password);
    common::audit_session(&state, &session, "member.services_activated", None, None).await?;
    Ok(Json(my_services(&state, session.subject.id).await?))
}

/// "Use my Starr password here" (M2): switch an adopted account to the
/// Starr password. Every device signed in to that account is signed out
/// when Starr writes it. Needs the current password; wrong ones count
/// against the login throttle.
#[utoipa::path(post, path = "/api/v1/me/services/{serviceId}/use-starr-password", tag = "me",
    params(("serviceId" = i64, Path, description = "Service id")),
    request_body = ConfirmPasswordRequest,
    responses(
        (status = 200, description = "Requested; the account switches shortly", body = MyServices),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not a member)", body = ErrorBody),
        (status = 404, description = "`services.not_found` (no account of the member there)", body = ErrorBody),
        (status = 409, description = "`services.already_starr_password`, `services.nothing_to_activate` (account writes are off there)", body = ErrorBody),
        (status = 422, description = "`auth.current_password_invalid`", body = ErrorBody),
        (status = 429, description = "`auth.too_many_attempts`", body = ErrorBody),
        (status = 503, description = "`services.secrets_unavailable` (the worker is not running yet)", body = ErrorBody),
    ))]
pub async fn use_starr_password(
    MemberSession(session): MemberSession,
    State(state): State<AppState>,
    client: Client,
    Path(service_id): Path<i64>,
    Json(body): Json<ConfirmPasswordRequest>,
) -> Result<Json<MyServices>, ApiError> {
    let password = checked_password(&state, &client, &session, body.password).await?;
    let member_id = session.subject.id;
    // Only where Starr writes now (live, the same rule as
    // `acceptsPassword`): nothing waits for a paused or plan-only server.
    let writes_on = state
        .db
        .account_writes(service_id)
        .await?
        .is_some_and(|w| w.mode() == WritesMode::Live);
    if !writes_on {
        return Err(ApiError::Conflict("services.nothing_to_activate"));
    }
    match state
        .db
        .request_password_switch(member_id, service_id)
        .await?
    {
        SwitchOutcome::NoAccount => return Err(ApiError::NotFound("services.not_found")),
        SwitchOutcome::AlreadyStarr => {
            return Err(ApiError::Conflict("services.already_starr_password"));
        }
        SwitchOutcome::Requested => {}
    }
    if let Err(err) = offered(&state, member_id, &password).await {
        // No handoff, no switch: the account keeps its old password.
        state
            .db
            .cancel_password_switch(member_id, service_id)
            .await?;
        return Err(err);
    }
    drop(password);
    common::audit_session(
        &state,
        &session,
        "member.password_switch_requested",
        None,
        Some(json!({ "serviceId": service_id })),
    )
    .await?;
    Ok(Json(my_services(&state, member_id).await?))
}

#[cfg(test)]
mod tests {
    use starr_core::members::RequestMode;
    use starr_core::provisioning::{HandsOffReason, WaitReason};

    use super::*;

    fn entry(state: PlanState) -> PlanEntry {
        PlanEntry {
            member_id: 1,
            state,
            remote_id: None,
            steps: vec![],
        }
    }

    fn shown(
        state: PlanState,
        live: bool,
        handoff: bool,
    ) -> (MyServiceStateDto, Option<WaitingForDto>) {
        shown_state(MemberView::SettingUp, Some(&entry(state)), live, handoff)
    }

    #[test]
    fn setting_up_splits_into_starr_at_work_and_an_admin_to_act() {
        use MyServiceStateDto::{SettingUp, Waiting};
        let taken = PlanState::Conflict {
            conflict: Conflict::NameTaken {
                remote_id: "x".into(),
            },
        };
        assert_eq!(
            shown(taken.clone(), true, false),
            (Waiting, Some(WaitingForDto::NameTaken))
        );
        // A conflict waits for the admin even with a password on its way.
        assert_eq!(
            shown(taken, true, true),
            (Waiting, Some(WaitingForDto::NameTaken))
        );
        assert_eq!(
            shown(PlanState::LockedByService, true, false),
            (Waiting, Some(WaitingForDto::Locked))
        );
        let privileged = PlanState::Conflict {
            conflict: Conflict::Privileged,
        };
        assert_eq!(
            shown(privileged, true, false),
            (Waiting, Some(WaitingForDto::Admin))
        );
        let unmapped = PlanState::Waiting {
            reason: WaitReason::LibrariesUnmapped,
        };
        assert_eq!(
            shown(unmapped, true, false),
            (Waiting, Some(WaitingForDto::Admin))
        );
        // A handoff in flight is Starr at work, writes or not.
        assert_eq!(shown(PlanState::ToCreate, false, true), (SettingUp, None));
        assert_eq!(
            shown(PlanState::ToCreate, false, false),
            (Waiting, Some(WaitingForDto::WritesOff))
        );
        assert_eq!(shown(PlanState::Managed, true, false), (SettingUp, None));
        // Decided, not planned yet: no entry, Starr is on it.
        assert_eq!(
            shown_state(MemberView::SettingUp, None, true, false),
            (SettingUp, None)
        );
        // Other views pass through without a reason.
        assert_eq!(
            shown_state(
                MemberView::Ready,
                Some(&entry(PlanState::HandsOff {
                    reason: HandsOffReason::NoAccount
                })),
                true,
                false
            ),
            (MyServiceStateDto::Ready, None)
        );
    }

    #[test]
    fn only_a_public_category_lists_an_undecided_service() {
        let on = |category| Entitlement {
            category,
            enabled: true,
            request_mode: RequestMode::Approval,
        };
        let enabled = [Category::Movies, Category::Adult];
        // Entitled to the private category only: the service stays unlisted.
        assert!(!has_public_category(
            [Category::Adult].into_iter(),
            &[on(Category::Adult)],
            &enabled
        ));
        assert!(has_public_category(
            [Category::Adult, Category::Movies].into_iter(),
            &[on(Category::Adult), on(Category::Movies)],
            &enabled
        ));
        // A category switched off counts as none.
        assert!(!has_public_category(
            [Category::Series].into_iter(),
            &[on(Category::Series)],
            &enabled
        ));
    }
}
