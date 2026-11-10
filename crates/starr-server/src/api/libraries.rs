//! Libraries per category (spec 0005 M4, slice 2), admins only and
//! read-only towards the services:
//! - `/api/v1/services/{id}/libraries`: an instance's libraries as last
//!   listed by `accounts.scan` (refresh: `POST /api/v1/jobs/accounts.scan/trigger`),
//!   with the category each belongs to;
//! - `/api/v1/libraries/unmapped`: libraries no category maps and no admin
//!   ignored ("needs you");
//! - `/api/v1/members/{id}/libraries`: which libraries a member would get
//!   per consume instance (derived, nothing is written yet).
//!
//! The mapping itself is saved per consume role
//! (`PUT /api/v1/categories/{category}/roles/{id}/libraries`).

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, State};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::adapters::Capability;
use starr_core::categories::RoleKind;
use starr_core::libraries::{self, ConsumeSettings, RoleLibraries};
use starr_core::library::{LibraryContent, MediaLibrary};
use starr_core::members::Category;
use starr_db::audit::{ActorKind, AuditEntry};
use starr_db::categories::ServiceRole;
use starr_db::libraries::ServiceLibrary;
use starr_db::services::ServiceInstance;
use starr_worker::adapters;
use utoipa::ToSchema;

use super::common::CategoryDto;
use super::error::{ApiError, ErrorBody};
use crate::AppState;
use crate::auth::AdminSession;

/// What a library holds, as the service reports its type.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = LibraryContent)]
pub enum LibraryContentDto {
    Movies,
    Series,
    Music,
    Books,
    /// Mixed, home videos, … or a type Starr does not know.
    Other,
}

impl From<LibraryContent> for LibraryContentDto {
    fn from(c: LibraryContent) -> Self {
        match c {
            LibraryContent::Movies => Self::Movies,
            LibraryContent::Series => Self::Series,
            LibraryContent::Music => Self::Music,
            LibraryContent::Books => Self::Books,
            LibraryContent::Other => Self::Other,
        }
    }
}

/// One library of an instance.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryDto {
    /// The service's library ID (the one the mapping names).
    id: String,
    name: String,
    content: LibraryContentDto,
    /// The category it belongs to on this instance; absent: none.
    category: Option<CategoryDto>,
    /// Since when an admin marked it as belonging to no category.
    ignored_at: Option<DateTime<Utc>>,
    ignored_by: Option<String>,
}

/// The libraries of one consume instance as last listed.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceLibraries {
    service_id: i64,
    /// Last listing attempt; absent if none ran yet.
    listed_at: Option<DateTime<Utc>>,
    /// Whether the last successful listing was provably complete; absent
    /// when the last attempt failed or none ran.
    complete: Option<bool>,
    /// Error code of the last attempt (`credentials_invalid`, `transient`,
    /// …); absent when it succeeded. The libraries of the last successful
    /// listing stay.
    error: Option<String>,
    /// By name, ignoring case.
    libraries: Vec<LibraryDto>,
}

/// A library no category maps on its instance and no admin ignored.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UnmappedLibrary {
    pub(crate) service_id: i64,
    service_name: String,
    adapter_type: String,
    pub(crate) library_id: String,
    name: String,
    content: LibraryContentDto,
}

/// Mark a library as belonging to no category, or undo that.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetLibraryIgnored {
    ignored: bool,
}

/// A library a member would get, by name if the instance still lists it.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GrantedLibrary {
    id: String,
    /// Absent if the last listing no longer has it.
    name: Option<String>,
    category: CategoryDto,
}

/// What a member would get on one consume instance.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemberInstanceLibraries {
    service_id: i64,
    service_name: String,
    /// The explicit list (never "all libraries"); empty: no category of the
    /// member is watched in here, so no account.
    libraries: Vec<GrantedLibrary>,
    /// Categories the member has here whose consume role maps no library
    /// yet: they give nothing until an admin maps libraries.
    unmapped_categories: Vec<CategoryDto>,
}

/// The consume roles of every instance, by instance id.
pub(crate) async fn consume_roles(
    state: &AppState,
) -> Result<HashMap<i64, Vec<(ServiceRole, ConsumeSettings)>>, ApiError> {
    let mut by_instance: HashMap<i64, Vec<(ServiceRole, ConsumeSettings)>> = HashMap::new();
    for role in state.db.list_roles(None).await? {
        if role.kind == RoleKind::Consume {
            let settings = role.consume_settings()?;
            by_instance
                .entry(role.service_instance_id)
                .or_default()
                .push((role, settings));
        }
    }
    Ok(by_instance)
}

fn role_libraries(roles: &[(ServiceRole, ConsumeSettings)]) -> Vec<RoleLibraries<'_>> {
    roles
        .iter()
        .map(|(role, settings)| RoleLibraries {
            category: role.category,
            libraries: &settings.libraries,
        })
        .collect()
}

/// The instance, if it exists and its adapter has libraries.
async fn library_instance(state: &AppState, id: i64) -> Result<ServiceInstance, ApiError> {
    let service = state
        .db
        .get_service(id)
        .await?
        .ok_or(ApiError::NotFound("services.not_found"))?;
    let has_libraries = adapters::descriptor(&service.adapter_type)
        .is_some_and(|d| d.capabilities.contains(&Capability::Libraries));
    if !has_libraries {
        return Err(ApiError::Validation("libraries.unsupported"));
    }
    Ok(service)
}

fn to_dto(row: ServiceLibrary, roles: &[(ServiceRole, ConsumeSettings)]) -> LibraryDto {
    let category = roles
        .iter()
        .find(|(_, s)| s.libraries.iter().any(|m| m.id == row.library.id))
        .map(|(r, _)| r.category.into());
    LibraryDto {
        id: row.library.id,
        name: row.library.name,
        content: row.library.content.into(),
        category,
        ignored_at: row.ignored_at,
        ignored_by: row.ignored_by,
    }
}

/// The libraries of a consume instance as last listed, with the category
/// each belongs to (admins only).
#[utoipa::path(get, path = "/api/v1/services/{id}/libraries", tag = "libraries",
    params(("id" = i64, Path, description = "Service id")),
    responses(
        (status = 200, body = ServiceLibraries),
        (status = 404, body = ErrorBody, description = "`services.not_found`"),
        (status = 422, body = ErrorBody, description = "`libraries.unsupported` (the service has no libraries)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_service_libraries(
    _admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<ServiceLibraries>, ApiError> {
    library_instance(&state, id).await?;
    let listing = state
        .db
        .library_listing_state(id)
        .await?
        .ok_or(ApiError::NotFound("services.not_found"))?;
    let mut roles = consume_roles(&state).await?;
    let roles = roles.remove(&id).unwrap_or_default();
    let libraries = state
        .db
        .service_libraries(id)
        .await?
        .into_iter()
        .map(|row| to_dto(row, &roles))
        .collect();
    Ok(Json(ServiceLibraries {
        service_id: id,
        listed_at: listing.listed_at,
        complete: listing.complete,
        error: listing.error,
        libraries,
    }))
}

/// Mark a library as belonging to no category (it no longer shows as
/// unmapped) or undo that. Starr-side only; audited when it changes
/// something.
#[utoipa::path(put, path = "/api/v1/services/{id}/libraries/{libraryId}/ignored", tag = "libraries",
    params(
        ("id" = i64, Path, description = "Service id"),
        ("libraryId" = String, Path, description = "The service's library ID"),
    ),
    request_body = SetLibraryIgnored,
    responses(
        (status = 200, body = LibraryDto),
        (status = 404, body = ErrorBody, description = "`services.not_found`, `libraries.not_found`"),
        (status = 422, body = ErrorBody, description = "`libraries.unsupported` (the service has no libraries)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn set_library_ignored(
    admin: AdminSession,
    State(state): State<AppState>,
    Path((id, library_id)): Path<(i64, String)>,
    Json(body): Json<SetLibraryIgnored>,
) -> Result<Json<LibraryDto>, ApiError> {
    let service = library_instance(&state, id).await?;
    let username = &admin.0.subject.username;
    let now = state.clock.now();
    let (row, changed) = state
        .db
        .set_library_ignored(id, &library_id, body.ignored.then_some(username), now)
        .await?
        .ok_or(ApiError::NotFound("libraries.not_found"))?;
    let mut roles = consume_roles(&state).await?;
    let roles = roles.remove(&id).unwrap_or_default();
    if changed {
        let action = if body.ignored {
            "libraries.ignore"
        } else {
            "libraries.unignore"
        };
        let target = format!("{}:{}", service.name, row.library.name);
        let details = json!({ "serviceId": id, "libraryId": row.library.id }).to_string();
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
    }
    Ok(Json(to_dto(row, &roles)))
}

/// Libraries of consume instances that no category maps and no admin
/// ignored, by instance and name ("unmapped library" under "needs you").
#[utoipa::path(get, path = "/api/v1/libraries/unmapped", tag = "libraries",
    responses(
        (status = 200, body = Vec<UnmappedLibrary>),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_unmapped_libraries(
    _admin: AdminSession,
    State(state): State<AppState>,
) -> Result<Json<Vec<UnmappedLibrary>>, ApiError> {
    Ok(Json(unmapped_libraries(&state).await?))
}

/// The unmapped libraries of every consume instance (also the Inbox's
/// `library_unmapped` items).
pub(crate) async fn unmapped_libraries(state: &AppState) -> Result<Vec<UnmappedLibrary>, ApiError> {
    let roles = consume_roles(state).await?;
    let mut ids: Vec<i64> = roles.keys().copied().collect();
    ids.sort_unstable();
    let mut out = Vec::new();
    for id in ids {
        let Some(service) = state.db.get_service(id).await? else {
            continue;
        };
        let rows = state.db.service_libraries(id).await?;
        let listed: Vec<MediaLibrary> = rows
            .iter()
            .filter(|r| r.ignored_at.is_none())
            .map(|r| r.library.clone())
            .collect();
        let instance_roles = roles.get(&id).map(Vec::as_slice).unwrap_or_default();
        for library in libraries::unmapped(&listed, &role_libraries(instance_roles)) {
            out.push(UnmappedLibrary {
                service_id: id,
                service_name: service.name.clone(),
                adapter_type: service.adapter_type.clone(),
                library_id: library.id.clone(),
                name: library.name.clone(),
                content: library.content.into(),
            });
        }
    }
    Ok(out)
}

/// The libraries a member would get on each consume instance, derived from
/// the member's entitlements, the switched-on categories and the library
/// mapping (spec 0005 M4). Read-only: nothing is written to any service
/// before slice 4.
#[utoipa::path(get, path = "/api/v1/members/{id}/libraries", tag = "libraries",
    params(("id" = i64, Path, description = "Member id")),
    responses(
        (status = 200, body = Vec<MemberInstanceLibraries>),
        (status = 404, body = ErrorBody, description = "`members.not_found`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn member_libraries(
    _admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<MemberInstanceLibraries>>, ApiError> {
    state
        .db
        .get_member(id)
        .await?
        .ok_or(ApiError::NotFound("members.not_found"))?;
    let entitlements = state.db.entitlements(id).await?;
    let enabled: Vec<Category> = state
        .db
        .list_categories()
        .await?
        .into_iter()
        .filter(|c| c.enabled)
        .map(|c| c.category)
        .collect();
    let roles = consume_roles(&state).await?;
    let mut ids: Vec<i64> = roles.keys().copied().collect();
    ids.sort_unstable();
    let mut out = Vec::new();
    for instance in ids {
        let instance_roles = roles.get(&instance).map(Vec::as_slice).unwrap_or_default();
        let Some((first, _)) = instance_roles.first() else {
            continue;
        };
        let names: HashMap<String, String> = state
            .db
            .service_libraries(instance)
            .await?
            .into_iter()
            .map(|r| (r.library.id, r.library.name))
            .collect();
        let desired = libraries::desired(&entitlements, &enabled, &role_libraries(instance_roles));
        // Every desired library comes from one of these roles.
        let granted = desired
            .into_iter()
            .filter_map(|m| {
                let (role, _) = instance_roles
                    .iter()
                    .find(|(_, s)| s.libraries.contains(&m))?;
                Some(GrantedLibrary {
                    name: names.get(&m.id).cloned(),
                    id: m.id,
                    category: role.category.into(),
                })
            })
            .collect();
        // One consume role per category and instance: no duplicates.
        let unmapped_categories: Vec<CategoryDto> = instance_roles
            .iter()
            .filter(|(r, s)| {
                s.libraries.is_empty() && libraries::entitled(&entitlements, &enabled, r.category)
            })
            .map(|(r, _)| r.category.into())
            .collect();
        out.push(MemberInstanceLibraries {
            service_id: instance,
            service_name: first.service_name.clone(),
            libraries: granted,
            unmapped_categories,
        });
    }
    Ok(Json(out))
}
