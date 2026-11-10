//! Foreign account inventory per consume instance (spec 0005 slice 1,
//! D7): `/api/v1/services/{id}/accounts` (admins only, read-only towards
//! the services). Scans run as `accounts.scan` jobs in the worker
//! (`POST /api/v1/jobs/accounts.scan/trigger`); ignoring an account is a
//! Starr-side flag and writes nothing to the service.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, State};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::accounts::suggested_member;
use starr_db::audit::{ActorKind, AuditEntry};
use starr_db::members::{MemberFilter, MemberRecord};
use starr_db::remote_accounts::RemoteAccountRow;
use starr_db::services::ServiceInstance;
use starr_worker::adapters;
use utoipa::ToSchema;

use super::error::{ApiError, ErrorBody};
use crate::AppState;
use crate::auth::AdminSession;

/// The member a foreign account is suggested for (equal username, ignoring
/// case). Only a suggestion: nothing is linked.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SuggestedMember {
    id: i64,
    username: String,
}

/// One account the service listed.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)] // independent facts of one account
pub struct ForeignAccount {
    /// The service's account ID.
    remote_id: String,
    /// The name in the service.
    username: String,
    /// The service's enabled flag (a Jellyfin lockout also disables).
    enabled: bool,
    /// Administrator in the service; Starr never manages it.
    privileged: bool,
    /// Locked by the service after failed logins (Jellyfin); never lifted
    /// by Starr.
    locked_by_service: bool,
    /// The service's IDs of the libraries the account may see; absent:
    /// every library, including future ones.
    library_ids: Option<Vec<String>>,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
    /// Complete listings in a row that missed the account.
    missing_runs: u32,
    /// Since when the account is gone (missing from two complete
    /// listings); purged 30 days later.
    gone_since: Option<DateTime<Utc>>,
    /// Since when an admin marked the account ignored (Starr-side only).
    ignored_at: Option<DateTime<Utc>>,
    ignored_by: Option<String>,
    /// Never for privileged, gone, ignored or linked accounts.
    suggested_member: Option<SuggestedMember>,
    /// The member this account is linked to (Starr owns it for them).
    linked_member: Option<SuggestedMember>,
    /// Starr locked the account with a random password (a service without
    /// a disable flag, Navidrome, which still reports it as enabled).
    locked_by_starr: bool,
}

/// The accounts of one consume instance and its last scan.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountInventory {
    service_id: i64,
    /// Absent until the first scan finished.
    scanned_at: Option<DateTime<Utc>>,
    /// Whether the last successful listing was provably complete; absent
    /// when the last scan failed or none ran.
    scan_complete: Option<bool>,
    /// Error code of the last scan (`credentials_invalid`, `transient`,
    /// `throttled`, `rejected`, `unknown`, `credentials_unreadable`, …);
    /// absent when it succeeded.
    scan_error: Option<String>,
    /// By name, ignoring case.
    accounts: Vec<ForeignAccount>,
}

/// Mark an account ignored or not.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetIgnored {
    ignored: bool,
}

fn to_dto(
    row: RemoteAccountRow,
    members: &[MemberRecord],
    owners: &HashMap<String, i64>,
) -> ForeignAccount {
    let linked_member = owners
        .get(&row.remote_id)
        .and_then(|id| members.iter().find(|m| m.id == *id))
        .map(|m| SuggestedMember {
            id: m.id,
            username: m.username.clone(),
        });
    let hands_off = row.privileged
        || row.gone_since.is_some()
        || row.ignored_at.is_some()
        || owners.contains_key(&row.remote_id);
    let suggested = if hands_off {
        None
    } else {
        suggested_member(&row.username, row.privileged, members, |m| {
            m.username.as_str()
        })
        .map(|m| SuggestedMember {
            id: m.id,
            username: m.username.clone(),
        })
    };
    ForeignAccount {
        remote_id: row.remote_id,
        username: row.username,
        enabled: row.enabled,
        privileged: row.privileged,
        locked_by_service: row.locked_by_service,
        library_ids: row.libraries,
        first_seen: row.first_seen,
        last_seen: row.last_seen,
        missing_runs: row.missing_runs,
        gone_since: row.gone_since,
        ignored_at: row.ignored_at,
        ignored_by: row.ignored_by,
        suggested_member: suggested,
        linked_member,
        locked_by_starr: row.locked_by_starr,
    }
}

/// Remote ID → the member who owns the account, for one instance.
async fn owners(state: &AppState, instance_id: i64) -> Result<HashMap<String, i64>, ApiError> {
    Ok(state
        .db
        .service_accounts(instance_id)
        .await?
        .into_iter()
        .filter_map(|r| Some((r.remote_id?, r.member_id)))
        .collect())
}

/// The instance, if it exists and its adapter reads accounts.
async fn account_instance(state: &AppState, id: i64) -> Result<ServiceInstance, ApiError> {
    let service = state
        .db
        .get_service(id)
        .await?
        .ok_or(ApiError::NotFound("services.not_found"))?;
    if !adapters::has_accounts(&service.adapter_type) {
        return Err(ApiError::Validation("accounts.unsupported"));
    }
    Ok(service)
}

/// Every account the service listed, with suggestions (admins only).
#[utoipa::path(get, path = "/api/v1/services/{id}/accounts", tag = "accounts",
    params(("id" = i64, Path, description = "Service id")),
    responses(
        (status = 200, body = AccountInventory),
        (status = 404, body = ErrorBody, description = "`services.not_found`"),
        (status = 422, body = ErrorBody, description = "`accounts.unsupported` (the service has no accounts)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_accounts(
    _admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<AccountInventory>, ApiError> {
    account_instance(&state, id).await?;
    let scan = state
        .db
        .account_scan_state(id)
        .await?
        .ok_or(ApiError::NotFound("services.not_found"))?;
    let members = state.db.list_members(MemberFilter::default()).await?;
    let owners = owners(&state, id).await?;
    let accounts = state
        .db
        .remote_accounts(id)
        .await?
        .into_iter()
        .map(|row| to_dto(row, &members, &owners))
        .collect();
    Ok(Json(AccountInventory {
        service_id: id,
        scanned_at: scan.scanned_at,
        scan_complete: scan.complete,
        scan_error: scan.error,
        accounts,
    }))
}

/// Mark an account ignored (Starr leaves it alone and suggests nothing for
/// it) or not. Starr-side only: nothing is written to the service. Audited
/// when it changes something; repeating a call is a no-op.
#[utoipa::path(put, path = "/api/v1/services/{id}/accounts/{remoteId}/ignored", tag = "accounts",
    params(
        ("id" = i64, Path, description = "Service id"),
        ("remoteId" = String, Path, description = "The service's account ID"),
    ),
    request_body = SetIgnored,
    responses(
        (status = 200, body = ForeignAccount),
        (status = 404, body = ErrorBody, description = "`services.not_found`, `accounts.not_found`"),
        (status = 409, body = ErrorBody, description = "`accounts.linked` (linked to a member; unlink first)"),
        (status = 422, body = ErrorBody, description = "`accounts.unsupported` (the service has no accounts)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn set_ignored(
    admin: AdminSession,
    State(state): State<AppState>,
    Path((id, remote_id)): Path<(i64, String)>,
    Json(body): Json<SetIgnored>,
) -> Result<Json<ForeignAccount>, ApiError> {
    let service = account_instance(&state, id).await?;
    let owners = owners(&state, id).await?;
    if body.ignored && owners.contains_key(&remote_id) {
        return Err(ApiError::Conflict("accounts.linked"));
    }
    let username = &admin.0.subject.username;
    let now = state.clock.now();
    let (row, changed) = state
        .db
        .set_remote_account_ignored(id, &remote_id, body.ignored.then_some(username), now)
        .await?
        .ok_or(ApiError::NotFound("accounts.not_found"))?;
    let members = state.db.list_members(MemberFilter::default()).await?;
    if !changed {
        return Ok(Json(to_dto(row, &members, &owners)));
    }
    let action = if body.ignored {
        "accounts.ignore"
    } else {
        "accounts.unignore"
    };
    let target = format!("{}:{}", service.name, row.username);
    let details = json!({ "serviceId": id, "remoteId": row.remote_id }).to_string();
    state
        .db
        .audit(
            &AuditEntry {
                actor_kind: ActorKind::Admin,
                actor_name: Some(username),
                action,
                target: Some(&target),
                details: Some(&details),
            },
            now,
        )
        .await?;
    super::service_accounts::accounts_changed(&state).await?;
    Ok(Json(to_dto(row, &members, &owners)))
}
