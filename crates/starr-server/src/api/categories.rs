//! Categories and their service roles: `/api/v1/categories` (admins).
//!
//! Per category: on/off, which instances serve as catalog, source and
//! consume (domain model §3), the default catalog's relevance (D3) and the
//! source's quality profile, root folder and minimum availability (D9).
//! Every category can be switched on and get consume roles; catalog roles
//! need a catalog adapter (movies, series, music), source roles a source
//! adapter (every category; Lidarr since spec 0006 slice 3, Whisparr for
//! adult since spec 0007 slice 3). Music requests open with spec 0006
//! slice 4, adult requests with spec 0007 slice 4 (ADR 0012).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::catalog::{Catalog, Relevance};
use starr_core::categories::{
    MinimumAvailability, RoleKind, SourceSettings, has_catalog, has_request_flow, has_source,
    is_private, media_kind, role_allowed_in, validate_music_relevance, validate_relevance,
};
use starr_core::jobs::{JobKind, Trigger};
use starr_core::libraries::{ConsumeSettings, RoleLibraries, validate_mapping};
use starr_core::members::Category;
use starr_db::audit::{ActorKind, AuditEntry};
use starr_db::categories::{NewRole, ServiceRole};
use starr_db::jobs::NewJob;
use starr_worker::adapters;
use utoipa::{IntoParams, ToSchema};

use super::catalog::{RelevanceDto, adapter_error, open_catalog};
use super::common::{CategoryDto, ConfirmRequired, Effect};
use super::error::{ApiError, ErrorBody};
use crate::AppState;
use crate::auth::AdminSession;

/// What a service does for a category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = RoleKind)]
pub enum RoleKindDto {
    Catalog,
    Source,
    Consume,
}

impl From<RoleKindDto> for RoleKind {
    fn from(k: RoleKindDto) -> Self {
        match k {
            RoleKindDto::Catalog => Self::Catalog,
            RoleKindDto::Source => Self::Source,
            RoleKindDto::Consume => Self::Consume,
        }
    }
}

/// When Radarr considers a movie available (its `minimumAvailability`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = MinimumAvailability)]
pub enum MinimumAvailabilityDto {
    Announced,
    InCinemas,
    Released,
}

impl From<MinimumAvailabilityDto> for MinimumAvailability {
    fn from(m: MinimumAvailabilityDto) -> Self {
        match m {
            MinimumAvailabilityDto::Announced => Self::Announced,
            MinimumAvailabilityDto::InCinemas => Self::InCinemas,
            MinimumAvailabilityDto::Released => Self::Released,
        }
    }
}

impl From<MinimumAvailability> for MinimumAvailabilityDto {
    fn from(m: MinimumAvailability) -> Self {
        match m {
            MinimumAvailability::Announced => Self::Announced,
            MinimumAvailability::InCinemas => Self::InCinemas,
            MinimumAvailability::Released => Self::Released,
        }
    }
}

/// Quality profile, root folder, (movies) minimum availability and (music)
/// metadata profile of a category's source.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSettingsDto {
    /// Id from the source's quality profiles (`/source-lists`).
    #[serde(default)]
    quality_profile_id: Option<i64>,
    /// Path from the source's root folders (`/source-lists`).
    #[serde(default)]
    root_folder_path: Option<String>,
    /// Movies only; Radarr's default for new movies is `released`.
    #[serde(default)]
    minimum_availability: Option<MinimumAvailabilityDto>,
    /// Music only: id from the source's metadata profiles
    /// (`/source-lists`), the profile Lidarr gives a new artist.
    #[serde(default)]
    metadata_profile_id: Option<i64>,
}

/// A service in a role.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoleDto {
    id: i64,
    service_id: i64,
    service_name: String,
    adapter_type: String,
}

impl From<&ServiceRole> for RoleDto {
    fn from(r: &ServiceRole) -> Self {
        Self {
            id: r.id,
            service_id: r.service_instance_id,
            service_name: r.service_name.clone(),
            adapter_type: r.adapter_type.clone(),
        }
    }
}

/// A library mapped to the category on a consume instance.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MappedLibraryDto {
    /// The service's library ID.
    id: String,
    /// Absent if the instance's last library listing no longer has it
    /// (removed, or renamed in Jellyfin, which gives it a new ID).
    name: Option<String>,
}

/// A consume instance of the category with its library mapping.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerRoleDto {
    id: i64,
    service_id: i64,
    service_name: String,
    adapter_type: String,
    /// The instance's libraries that belong to the category. Empty: none
    /// mapped yet; availability then counts every library not mapped to
    /// another category, and members get no library here (spec 0005 M4).
    libraries: Vec<MappedLibraryDto>,
}

/// The libraries of a consume instance that belong to the category.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetRoleLibraries {
    /// Library IDs from `/api/v1/services/{id}/libraries`; may be empty.
    library_ids: Vec<String>,
}

/// Most libraries one role may map (a guard on the request size).
const MAX_ROLE_LIBRARIES: usize = 200;

/// A catalog of the category.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogRoleDto {
    id: i64,
    service_id: i64,
    service_name: String,
    adapter_type: String,
    /// The catalog Discover and search use.
    is_default: bool,
    /// Absent until the admin saved one; Discover needs it (D3).
    relevance: Option<RelevanceDto>,
    /// The private category's catalog only: automatic requests per
    /// performer or studio follow and week (spec 0007 Q4; 10 until set).
    /// Absent for other catalogs.
    follow_cap_per_week: Option<u32>,
}

/// The weekly follow cap of the private category's catalog.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetFollowCap {
    /// Automatic requests per follow and week, 1 to 100.
    per_week: u32,
}

/// The category's source.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceRoleDto {
    id: i64,
    service_id: i64,
    service_name: String,
    adapter_type: String,
    settings: SourceSettingsDto,
}

/// A category with its roles and settings.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
// Independent facts the admin UI shows, not a state machine in disguise.
#[allow(clippy::struct_excessive_bools)]
pub struct CategorySettings {
    category: CategoryDto,
    enabled: bool,
    /// Catalog and source adapters exist for the category, so it can have
    /// requests. Without (adult until spec 0007 slice 4; its source can be
    /// set up before) it can still be switched on and get consume roles.
    request_flow: bool,
    /// A source adapter exists for the category, so it can have a source
    /// (every category; adult with Whisparr since spec 0007 slice 3).
    source_flow: bool,
    /// A catalog adapter exists for the category, so it can have catalogs
    /// and Discover (movies, series, music).
    catalog_flow: bool,
    /// Switched on and the default catalog has a saved relevance (D3).
    discover_ready: bool,
    /// The category has requests and a source with a quality profile and a
    /// root folder (music: and a metadata profile): what a request needs
    /// (D9). Until then requests are refused with
    /// `requests.category_not_ready`.
    requests_ready: bool,
    catalogs: Vec<CatalogRoleDto>,
    source: Option<SourceRoleDto>,
    consumers: Vec<ConsumerRoleDto>,
    /// Where a relevance starts from (series hide Talk, News, Soap and
    /// Reality by default, D4).
    default_relevance: RelevanceDto,
    /// Adult: the operator statement it was switched on with. Switched on
    /// without one (before this release, or toggled by an older one), the
    /// private category stays closed for everybody until an admin confirms
    /// it by switching on again with `operatorStatement`.
    operator_statement: Option<OperatorStatementDto>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateCategory {
    enabled: bool,
    /// Adult only, required to switch it on (spec 0007 Q12): the admin
    /// confirms that adult content is legal where this server runs, that
    /// it is enabled only for adults who want it, and that they are
    /// responsible for the content and sources they connect. Asked again
    /// after every switch-off.
    #[serde(default)]
    operator_statement: bool,
}

/// The operator statement of the adult category: who confirmed it, when.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperatorStatementDto {
    at: chrono::DateTime<chrono::Utc>,
    /// The admin's username.
    by: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddRole {
    kind: RoleKindDto,
    service_id: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetSource {
    service_id: i64,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct SwitchQuery {
    /// The instance that would become the source.
    service_id: i64,
}

/// What switching the source would do. Open requests finish at the old
/// source (domain model §3).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceSwitchPreview {
    current: Option<RoleDto>,
    next_service_id: i64,
    next_service_name: String,
    /// Open requests that keep finishing at the current source.
    affected_requests: Vec<i64>,
}

/// The relevance a new catalog of `category` starts from.
fn default_relevance(category: Category) -> Relevance {
    match category {
        Category::Series => Relevance {
            excluded_genres: starr_adapter_tmdb::DEFAULT_SERIES_EXCLUDED_GENRES.to_vec(),
            ..Relevance::default()
        },
        // Albums and EPs, no secondary types (spec 0006 Q3).
        Category::Music => Relevance::music_default(),
        Category::Movies | Category::Adult => Relevance::default(),
    }
}

fn parse_category(raw: &str) -> Result<Category, ApiError> {
    Category::parse(raw).ok_or(ApiError::NotFound("categories.not_found"))
}

async fn settings_of(state: &AppState, category: Category) -> Result<CategorySettings, ApiError> {
    let stored = state.db.category_state(category).await?;
    let enabled = stored.enabled;
    let operator_statement = stored
        .statement
        .map(|s| OperatorStatementDto { at: s.at, by: s.by });
    let roles = state.db.list_roles(Some(category)).await?;
    let mut catalogs = Vec::new();
    let mut source = None;
    let mut consumers = Vec::new();
    for role in &roles {
        match role.kind {
            RoleKind::Catalog => catalogs.push(CatalogRoleDto {
                id: role.id,
                service_id: role.service_instance_id,
                service_name: role.service_name.clone(),
                adapter_type: role.adapter_type.clone(),
                is_default: role.is_default,
                relevance: role.relevance()?.map(RelevanceDto::from),
                follow_cap_per_week: offers_follow_cap(role).then(|| {
                    role.follow_cap_per_week
                        .unwrap_or(starr_core::follows::DEFAULT_CAP_PER_WEEK)
                }),
            }),
            RoleKind::Source => {
                let s = role.source_settings()?;
                source = Some(SourceRoleDto {
                    id: role.id,
                    service_id: role.service_instance_id,
                    service_name: role.service_name.clone(),
                    adapter_type: role.adapter_type.clone(),
                    settings: SourceSettingsDto {
                        quality_profile_id: s.quality_profile_id,
                        root_folder_path: s.root_folder_path,
                        minimum_availability: s.minimum_availability.map(Into::into),
                        metadata_profile_id: s.metadata_profile_id,
                    },
                });
            }
            RoleKind::Consume => {
                let names: std::collections::HashMap<String, String> = state
                    .db
                    .service_libraries(role.service_instance_id)
                    .await?
                    .into_iter()
                    .map(|l| (l.library.id, l.library.name))
                    .collect();
                consumers.push(ConsumerRoleDto {
                    id: role.id,
                    service_id: role.service_instance_id,
                    service_name: role.service_name.clone(),
                    adapter_type: role.adapter_type.clone(),
                    libraries: role
                        .consume_settings()?
                        .libraries
                        .into_iter()
                        .map(|m| MappedLibraryDto {
                            name: names.get(&m.id).cloned(),
                            id: m.id,
                        })
                        .collect(),
                });
            }
        }
    }
    // StashDB has no relevance to save (spec 0007 slice 2): its catalog is
    // ready as soon as it is the default.
    let discover_ready = enabled
        && catalogs.iter().any(|c| {
            c.is_default
                && (c.relevance.is_some()
                    || c.adapter_type == starr_adapter_stashdb::DESCRIPTOR.type_id)
        });
    // The same rule requests are checked with, not a copy of it.
    let requests_ready = has_request_flow(category)
        && super::requests::source_target(state, category)
            .await?
            .is_some();
    Ok(CategorySettings {
        category: category.into(),
        enabled,
        request_flow: has_request_flow(category),
        source_flow: has_source(category),
        catalog_flow: has_catalog(category),
        discover_ready,
        requests_ready,
        catalogs,
        source,
        consumers,
        default_relevance: default_relevance(category).into(),
        operator_statement,
    })
}

async fn audit(
    state: &AppState,
    admin: &AdminSession,
    action: &str,
    category: Category,
    details: &serde_json::Value,
) -> Result<(), ApiError> {
    state
        .db
        .audit(
            &AuditEntry {
                actor_kind: ActorKind::Admin,
                actor_name: Some(&admin.0.subject.username),
                action,
                target: Some(category.as_str()),
                details: Some(&details.to_string()),
            },
            state.clock.now(),
        )
        .await?;
    Ok(())
}

/// Check that service `id` may take `kind` in `category`: the category may
/// have the role (catalogs need a catalog adapter, sources the request
/// flow), the service exists, its adapter serves the category, and
/// (checked again by the repository) it has the capability. Returns the
/// adapter's declared capabilities.
async fn role_candidate(
    state: &AppState,
    category: Category,
    kind: RoleKind,
    id: i64,
) -> Result<(&'static [starr_core::adapters::Capability], String), ApiError> {
    if !role_allowed_in(kind, category) {
        return Err(ApiError::Validation("categories.no_request_flow"));
    }
    let service = state
        .db
        .get_service(id)
        .await?
        .ok_or(ApiError::NotFound("services.not_found"))?;
    let descriptor = adapters::descriptor(&service.adapter_type)
        .ok_or(ApiError::Validation("validation.adapter_type"))?;
    if !kind.allowed_for(descriptor.capabilities) {
        return Err(ApiError::Validation("categories.capability_missing"));
    }
    if !adapters::serves(&service.adapter_type, category) {
        return Err(ApiError::Validation("categories.adapter_unsuited"));
    }
    Ok((descriptor.capabilities, service.name))
}

async fn load_role(state: &AppState, category: Category, id: i64) -> Result<ServiceRole, ApiError> {
    state
        .db
        .get_role(id)
        .await?
        .filter(|r| r.category == category)
        .ok_or(ApiError::NotFound("categories.role_not_found"))
}

/// Every category with its roles and settings.
#[utoipa::path(get, path = "/api/v1/categories", tag = "categories",
    responses(
        (status = 200, body = Vec<CategorySettings>),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_categories(
    _admin: AdminSession,
    State(state): State<AppState>,
) -> Result<Json<Vec<CategorySettings>>, ApiError> {
    let mut out = Vec::new();
    for category in Category::ALL {
        out.push(settings_of(&state, category).await?);
    }
    Ok(Json(out))
}

/// Switch a category on or off.
#[utoipa::path(patch, path = "/api/v1/categories/{category}", tag = "categories",
    params(("category" = String, Path, description = "`movies`, `series`, `music` or `adult`")),
    request_body = UpdateCategory,
    responses(
        (status = 200, body = CategorySettings),
        (status = 404, body = ErrorBody, description = "`categories.not_found`"),
        (status = 422, body = ErrorBody, description = "`categories.statement_required` (switching adult on needs `operatorStatement`)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn update_category(
    admin: AdminSession,
    State(state): State<AppState>,
    Path(category): Path<String>,
    Json(body): Json<UpdateCategory>,
) -> Result<Json<CategorySettings>, ApiError> {
    let category = parse_category(&category)?;
    let now = state.clock.now();
    // Adult only switches on together with the operator statement (spec
    // 0007 Q12 A); switching off clears it, so the next switch-on asks
    // again. Account provisioning is not touched by the statement.
    let statement = category == Category::Adult && body.enabled;
    if statement {
        if !body.operator_statement {
            return Err(ApiError::Validation("categories.statement_required"));
        }
        state
            .db
            .enable_with_statement(category, &admin.0.subject.username, now)
            .await?;
    } else {
        state
            .db
            .set_category_enabled(category, body.enabled, now)
            .await?;
    }
    if category == Category::Adult {
        // Switching adult off (or on again) closes every unlock at once;
        // nobody's session reopens by itself.
        state.db.lock_private_unlocks(None).await?;
    }
    let details = if statement {
        json!({ "enabled": body.enabled, "operatorStatement": true })
    } else {
        json!({ "enabled": body.enabled })
    };
    audit(&state, &admin, "categories.update", category, &details).await?;
    super::service_accounts::accounts_changed(&state).await?;
    if category == Category::Adult && !body.enabled {
        // Its cached catalog answers and images go at once (spec 0007 Q13).
        // The switch stands either way: the housekeeping run enqueued here
        // repeats the purge in the worker (unless one is running already;
        // then the next hourly one does), and every hourly one does while
        // the category stays off.
        if let Err(err) = super::privacy::purge_private_caches(&state).await {
            tracing::warn!(error = ?err, "purging the private caches failed");
        }
        if let Err(err) = enqueue_now(&state, JobKind::Housekeeping).await {
            tracing::warn!(error = ?err, "enqueueing housekeeping failed");
        }
    }
    Ok(Json(settings_of(&state, category).await?))
}

/// Run `kind` as soon as the worker gets to it (moving a queued run
/// earlier).
async fn enqueue_now(state: &AppState, kind: JobKind) -> Result<(), ApiError> {
    let now = chrono::Utc::now();
    let key = kind.unique_key();
    state
        .db
        .enqueue_job(
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
    state.wake.notify_one();
    Ok(())
}

/// Give a service a role in a category. The first catalog becomes the
/// default; a second source is refused (switch it with `PUT .../source`).
#[utoipa::path(post, path = "/api/v1/categories/{category}/roles", tag = "categories",
    params(("category" = String, Path, description = "`movies`, `series`, `music` or `adult`")),
    request_body = AddRole,
    responses(
        (status = 201, body = CategorySettings),
        (status = 404, body = ErrorBody, description = "`categories.not_found`, `services.not_found`"),
        (status = 409, body = ErrorBody, description = "`categories.source_taken` (switch instead), `categories.role_taken`"),
        (status = 422, body = ErrorBody, description = "`categories.no_request_flow` (a catalog for adult), `categories.capability_missing` (e.g. Radarr as catalog), `categories.adapter_unsuited` (e.g. Sonarr for movies, TMDB for music)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn add_role(
    admin: AdminSession,
    State(state): State<AppState>,
    Path(category): Path<String>,
    Json(body): Json<AddRole>,
) -> Result<(StatusCode, Json<CategorySettings>), ApiError> {
    let category = parse_category(&category)?;
    let kind = RoleKind::from(body.kind);
    let (offered, name) = role_candidate(&state, category, kind, body.service_id).await?;
    let role = state
        .db
        .add_role(
            &NewRole {
                category,
                kind,
                service_instance_id: body.service_id,
                offered,
            },
            state.clock.now(),
        )
        .await?;
    audit(
        &state,
        &admin,
        "categories.role_add",
        category,
        &json!({ "roleId": role.id, "kind": kind.as_str(), "serviceId": body.service_id, "serviceName": name }),
    )
    .await?;
    if kind == RoleKind::Consume {
        super::service_accounts::accounts_changed(&state).await?;
    }
    if kind == RoleKind::Source {
        // Settings kept for this instance come back; its lists check them.
        super::catalog::enqueue_source_lists(&state, body.service_id).await?;
    }
    Ok((
        StatusCode::CREATED,
        Json(settings_of(&state, category).await?),
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct RemoveRoleQuery {
    /// The admin agreed to the `effects` a previous attempt answered with
    /// (409 `categories.confirm_required`).
    #[serde(default)]
    confirm: bool,
}

/// What removing the source `role` of `category` does (spec 0008 ADM-02).
async fn source_removal_effects(
    state: &AppState,
    category: Category,
    role: &ServiceRole,
) -> Result<Vec<Effect>, ApiError> {
    let mut effects = Vec::new();
    let enabled = state.db.category_state(category).await?.enabled;
    if enabled && state.db.source_target(category).await?.is_some() {
        effects.push(Effect::RequestsClose {
            category: category.into(),
        });
    }
    let open = state
        .db
        .open_requests_at(role.service_instance_id, category)
        .await?
        .len();
    if open > 0 {
        effects.push(Effect::OpenRequestsFinish { count: open });
    }
    if role.source_settings()? != SourceSettings::default() {
        effects.push(Effect::SettingsKept {
            category: category.into(),
        });
    }
    Ok(effects)
}

/// Remove a role. Removing the default catalog makes the oldest remaining
/// catalog the default. Removing a source closes the category's requests
/// and keeps its settings for that instance (they come back when it is the
/// source again, spec 0008 ADM-02); when that has consequences, the first
/// attempt answers 409 with the `effects` and changes nothing: send it
/// again with `confirm=true`.
#[utoipa::path(delete, path = "/api/v1/categories/{category}/roles/{id}", tag = "categories",
    params(
        ("category" = String, Path, description = "Category"),
        ("id" = i64, Path, description = "Role id"),
        RemoveRoleQuery,
    ),
    responses(
        (status = 200, body = CategorySettings),
        (status = 409, body = ConfirmRequired, description = "`categories.confirm_required` (a source whose removal has `effects`; nothing changed)"),
        (status = 404, body = ErrorBody, description = "`categories.not_found`, `categories.role_not_found`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn remove_role(
    admin: AdminSession,
    State(state): State<AppState>,
    Path((category, id)): Path<(String, i64)>,
    Query(query): Query<RemoveRoleQuery>,
) -> Result<Result<Json<CategorySettings>, ConfirmRequired>, ApiError> {
    let category = parse_category(&category)?;
    let role = load_role(&state, category, id).await?;
    if role.kind == RoleKind::Source && !query.confirm {
        let effects = source_removal_effects(&state, category, &role).await?;
        if !effects.is_empty() {
            return Ok(Err(ConfirmRequired {
                code: "categories.confirm_required",
                effects,
            }));
        }
    }
    state
        .db
        .remove_role(id)
        .await?
        .ok_or(ApiError::NotFound("categories.role_not_found"))?;
    audit(
        &state,
        &admin,
        "categories.role_remove",
        category,
        &json!({ "roleId": id, "kind": role.kind.as_str(), "serviceId": role.service_instance_id, "serviceName": role.service_name }),
    )
    .await?;
    if role.kind == RoleKind::Consume {
        super::service_accounts::accounts_changed(&state).await?;
    }
    Ok(Ok(Json(settings_of(&state, category).await?)))
}

/// Make a catalog the category's default.
#[utoipa::path(post, path = "/api/v1/categories/{category}/roles/{id}/default", tag = "categories",
    params(
        ("category" = String, Path, description = "Category"),
        ("id" = i64, Path, description = "Role id of a catalog"),
    ),
    responses(
        (status = 200, body = CategorySettings),
        (status = 404, body = ErrorBody, description = "`categories.not_found`, `categories.role_not_found`"),
        (status = 422, body = ErrorBody, description = "`categories.not_a_catalog`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn set_default_catalog(
    admin: AdminSession,
    State(state): State<AppState>,
    Path((category, id)): Path<(String, i64)>,
) -> Result<Json<CategorySettings>, ApiError> {
    let category = parse_category(&category)?;
    let role = load_role(&state, category, id).await?;
    if role.kind != RoleKind::Catalog {
        return Err(ApiError::Validation("categories.not_a_catalog"));
    }
    state.db.set_default_catalog(id, state.clock.now()).await?;
    audit(
        &state,
        &admin,
        "categories.default_catalog",
        category,
        &json!({ "roleId": id, "serviceId": role.service_instance_id }),
    )
    .await?;
    Ok(Json(settings_of(&state, category).await?))
}

/// What switching the category's source to another instance would do.
#[utoipa::path(get, path = "/api/v1/categories/{category}/source-switch", tag = "categories",
    params(("category" = String, Path, description = "Category"), SwitchQuery),
    responses(
        (status = 200, body = SourceSwitchPreview),
        (status = 404, body = ErrorBody, description = "`categories.not_found`, `services.not_found`"),
        (status = 422, body = ErrorBody, description = "`categories.no_request_flow`, `categories.capability_missing`, `categories.adapter_unsuited`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn preview_source_switch(
    _admin: AdminSession,
    State(state): State<AppState>,
    Path(category): Path<String>,
    Query(query): Query<SwitchQuery>,
) -> Result<Json<SourceSwitchPreview>, ApiError> {
    let category = parse_category(&category)?;
    let (_, name) = role_candidate(&state, category, RoleKind::Source, query.service_id).await?;
    let current = state.db.category_source(category).await?;
    let affected_requests = match &current {
        Some(role) => {
            state
                .db
                .open_requests_at(role.service_instance_id, category)
                .await?
        }
        None => Vec::new(),
    };
    Ok(Json(SourceSwitchPreview {
        current: current.as_ref().map(RoleDto::from),
        next_service_id: query.service_id,
        next_service_name: name,
        affected_requests,
    }))
}

/// Set or switch the category's source. The old source's settings are kept
/// for that instance and the new one gets those kept for it, if any (spec
/// 0008 ADM-02); open acquisitions finish at the old instance with the
/// settings they were approved with.
#[utoipa::path(put, path = "/api/v1/categories/{category}/source", tag = "categories",
    params(("category" = String, Path, description = "Category")),
    request_body = SetSource,
    responses(
        (status = 200, body = CategorySettings),
        (status = 404, body = ErrorBody, description = "`categories.not_found`, `services.not_found`"),
        (status = 422, body = ErrorBody, description = "`categories.no_request_flow`, `categories.capability_missing`, `categories.adapter_unsuited`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn set_source(
    admin: AdminSession,
    State(state): State<AppState>,
    Path(category): Path<String>,
    Json(body): Json<SetSource>,
) -> Result<Json<CategorySettings>, ApiError> {
    let category = parse_category(&category)?;
    let (offered, name) =
        role_candidate(&state, category, RoleKind::Source, body.service_id).await?;
    let previous = state.db.category_source(category).await?;
    if previous
        .as_ref()
        .is_some_and(|p| p.service_instance_id == body.service_id)
    {
        return Ok(Json(settings_of(&state, category).await?));
    }
    state
        .db
        .replace_source(
            &NewRole {
                category,
                kind: RoleKind::Source,
                service_instance_id: body.service_id,
                offered,
            },
            state.clock.now(),
        )
        .await?;
    audit(
        &state,
        &admin,
        "categories.source_switch",
        category,
        &json!({
            "serviceId": body.service_id,
            "serviceName": name,
            "previousServiceId": previous.as_ref().map(|p| p.service_instance_id),
        }),
    )
    .await?;
    super::catalog::enqueue_source_lists(&state, body.service_id).await?;
    Ok(Json(settings_of(&state, category).await?))
}

/// Save a catalog's relevance, validated against the catalog's own
/// reference data (TMDB: languages, regions, providers, genres;
/// `MusicBrainz`: release types only).
#[utoipa::path(put, path = "/api/v1/categories/{category}/roles/{id}/relevance", tag = "categories",
    params(
        ("category" = String, Path, description = "Category"),
        ("id" = i64, Path, description = "Role id of a catalog"),
    ),
    request_body = RelevanceDto,
    responses(
        (status = 200, body = CategorySettings),
        (status = 404, body = ErrorBody, description = "`categories.not_found`, `categories.role_not_found`"),
        (status = 422, body = ErrorBody, description = "`categories.no_request_flow`, `categories.not_a_catalog`, `validation.relevance_language`, `validation.relevance_release_region`, `validation.relevance_provider_region`, `validation.relevance_provider`, `validation.relevance_genre`, `validation.relevance_too_many`, `validation.relevance_not_offered` (a field the catalog does not use)"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid` (reference data could not be read)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn set_relevance(
    admin: AdminSession,
    State(state): State<AppState>,
    Path((category, id)): Path<(String, i64)>,
    Json(body): Json<RelevanceDto>,
) -> Result<Json<CategorySettings>, ApiError> {
    let category = parse_category(&category)?;
    if !has_catalog(category) {
        return Err(ApiError::Validation("categories.no_request_flow"));
    }
    let role = load_role(&state, category, id).await?;
    if role.kind != RoleKind::Catalog {
        return Err(ApiError::Validation("categories.not_a_catalog"));
    }
    let relevance = if role.adapter_type == starr_adapter_musicbrainz::DESCRIPTOR.type_id {
        validate_music_relevance(Relevance::from(body))
            .map_err(|e| ApiError::Validation(e.code()))?
    } else if role.adapter_type == starr_adapter_stashdb::DESCRIPTOR.type_id {
        // StashDB has nothing to restrict (its weekly follow cap has its
        // own route and column): saving the empty relevance readies the
        // private area's Discover.
        let relevance = Relevance::from(body);
        if relevance != Relevance::default() {
            return Err(ApiError::Validation("validation.relevance_not_offered"));
        }
        relevance
    } else {
        tmdb_relevance(&state, category, &role, body).await?
    };
    let stored = serde_json::to_string(&relevance).map_err(|err| {
        tracing::error!(error = %err, "serializing a relevance failed");
        ApiError::Internal
    })?;
    state
        .db
        .set_role_settings(id, &stored, state.clock.now())
        .await?
        .ok_or(ApiError::NotFound("categories.role_not_found"))?;
    audit(
        &state,
        &admin,
        "categories.relevance",
        category,
        &json!({ "roleId": id, "relevance": relevance }),
    )
    .await?;
    Ok(Json(settings_of(&state, category).await?))
}

/// Whether catalog role `role` has a weekly follow cap: the private
/// category's default `StashDB` (spec 0007 Q4), the one `follows.check`
/// reads.
fn offers_follow_cap(role: &ServiceRole) -> bool {
    role.kind == RoleKind::Catalog
        && role.is_default
        && is_private(role.category)
        && role.adapter_type == starr_adapter_stashdb::DESCRIPTOR.type_id
}

/// Set the weekly cap of automatic requests per performer or studio
/// follow (spec 0007 Q4, admin story 12; default 10). It applies from the
/// next `follows.check` run on; requests already made stay.
#[utoipa::path(put, path = "/api/v1/categories/{category}/roles/{id}/follow-cap", tag = "categories",
    params(
        ("category" = String, Path, description = "Category"),
        ("id" = i64, Path, description = "Role id of a catalog"),
    ),
    request_body = SetFollowCap,
    responses(
        (status = 200, body = CategorySettings),
        (status = 404, body = ErrorBody, description = "`categories.not_found`, `categories.role_not_found`"),
        (status = 422, body = ErrorBody, description = "`categories.follow_cap_not_offered` (not the private category's catalog), `validation.follow_cap` (1 to 100)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn set_follow_cap(
    admin: AdminSession,
    State(state): State<AppState>,
    Path((category, id)): Path<(String, i64)>,
    Json(body): Json<SetFollowCap>,
) -> Result<Json<CategorySettings>, ApiError> {
    let category = parse_category(&category)?;
    let role = load_role(&state, category, id).await?;
    if !offers_follow_cap(&role) {
        return Err(ApiError::Validation("categories.follow_cap_not_offered"));
    }
    if !starr_core::follows::CAP_RANGE.contains(&body.per_week) {
        return Err(ApiError::Validation("validation.follow_cap"));
    }
    state
        .db
        .set_follow_cap(id, body.per_week, state.clock.now())
        .await?
        .ok_or(ApiError::NotFound("categories.role_not_found"))?;
    audit(
        &state,
        &admin,
        "categories.follow_cap",
        category,
        &json!({ "roleId": id, "perWeek": body.per_week }),
    )
    .await?;
    Ok(Json(settings_of(&state, category).await?))
}

/// A TMDB relevance, validated against the catalog's reference data and
/// the providers of the chosen region.
async fn tmdb_relevance(
    state: &AppState,
    category: Category,
    role: &ServiceRole,
    body: RelevanceDto,
) -> Result<Relevance, ApiError> {
    let kind = media_kind(category).ok_or(ApiError::Validation("categories.no_request_flow"))?;
    let catalog = open_catalog(state, role.service_instance_id).await?;
    let config = catalog
        .configuration()
        .await
        .map_err(|e| adapter_error(&e))?;
    let input = Relevance::from(body);
    let region = input
        .provider_region
        .as_deref()
        .map(|r| r.trim().to_ascii_uppercase())
        .filter(|r| !r.is_empty());
    let providers = match &region {
        Some(r) if config.provider_regions.iter().any(|p| &p.code == r) => catalog
            .providers(kind, r)
            .await
            .map_err(|e| adapter_error(&e))?,
        _ => Vec::new(),
    };
    validate_relevance(kind, input, &config, &providers).map_err(|e| ApiError::Validation(e.code()))
}

/// Save the source's quality profile, root folder and minimum
/// availability, validated against the lists last read from the source.
#[utoipa::path(put, path = "/api/v1/categories/{category}/source/settings", tag = "categories",
    params(("category" = String, Path, description = "Category")),
    request_body = SourceSettingsDto,
    responses(
        (status = 200, body = CategorySettings),
        (status = 404, body = ErrorBody, description = "`categories.not_found`, `categories.no_source`"),
        (status = 422, body = ErrorBody, description = "`validation.quality_profile`, `validation.root_folder`, `validation.metadata_profile` (not in the source's lists; refresh them), `validation.minimum_availability` (movies only), `validation.metadata_profile_not_offered` (a metadata profile for a source without them)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn set_source_settings(
    admin: AdminSession,
    State(state): State<AppState>,
    Path(category): Path<String>,
    Json(body): Json<SourceSettingsDto>,
) -> Result<Json<CategorySettings>, ApiError> {
    let category = parse_category(&category)?;
    let source = state
        .db
        .category_source(category)
        .await?
        .ok_or(ApiError::NotFound("categories.no_source"))?;
    let settings = validate_source_settings(&state, category, &source, body).await?;
    state
        .db
        .set_source_settings(source.id, &settings, state.clock.now())
        .await?
        .ok_or(ApiError::NotFound("categories.no_source"))?;
    audit(
        &state,
        &admin,
        "categories.source_settings",
        category,
        &json!({ "roleId": source.id, "settings": settings, "metadataProfileId": settings.metadata_profile_id }),
    )
    .await?;
    Ok(Json(settings_of(&state, category).await?))
}

/// Profiles and folder must be in the lists last read from the source
/// (lesson 32: read, not remembered); minimum availability is Radarr's,
/// so movies only (default `released`); a metadata profile only for a
/// source that has them (Lidarr).
async fn validate_source_settings(
    state: &AppState,
    category: Category,
    source: &ServiceRole,
    body: SourceSettingsDto,
) -> Result<SourceSettings, ApiError> {
    let lists = state.db.source_lists(source.service_instance_id).await?;
    let profiles = lists
        .as_ref()
        .map_or_default(|l| l.quality_profiles.as_slice());
    let folders = lists.as_ref().map_or_default(|l| l.root_folders.as_slice());
    let metadata_profiles = lists
        .as_ref()
        .map_or_default(|l| l.metadata_profiles.as_slice());
    if let Some(id) = body.metadata_profile_id {
        if !adapters::has_metadata_profiles(&source.adapter_type) {
            return Err(ApiError::Validation(
                "validation.metadata_profile_not_offered",
            ));
        }
        if !metadata_profiles.iter().any(|p| p.id == id) {
            return Err(ApiError::Validation("validation.metadata_profile"));
        }
    }
    if let Some(id) = body.quality_profile_id
        && !profiles.iter().any(|p| p.id == id)
    {
        return Err(ApiError::Validation("validation.quality_profile"));
    }
    let root_folder_path = body.root_folder_path.map(|p| p.trim().to_owned());
    if let Some(path) = &root_folder_path
        && !folders.iter().any(|f| &f.path == path)
    {
        return Err(ApiError::Validation("validation.root_folder"));
    }
    let minimum_availability = match (category, body.minimum_availability) {
        (Category::Movies, chosen) => {
            Some(chosen.map_or_else(MinimumAvailability::default, Into::into))
        }
        (_, None) => None,
        (_, Some(_)) => return Err(ApiError::Validation("validation.minimum_availability")),
    };
    Ok(SourceSettings {
        quality_profile_id: body.quality_profile_id,
        root_folder_path,
        minimum_availability,
        metadata_profile_id: body.metadata_profile_id,
    })
}

/// Save which libraries of a consume instance belong to the category,
/// validated against the instance's last library listing: each library
/// belongs to at most one category per instance (spec 0005 M4). Writes
/// nothing to the service.
#[utoipa::path(put, path = "/api/v1/categories/{category}/roles/{id}/libraries", tag = "categories",
    params(
        ("category" = String, Path, description = "Category"),
        ("id" = i64, Path, description = "Role id of a consume role"),
    ),
    request_body = SetRoleLibraries,
    responses(
        (status = 200, body = CategorySettings),
        (status = 404, body = ErrorBody, description = "`categories.not_found`, `categories.role_not_found`"),
        (status = 422, body = ErrorBody, description = "`categories.not_a_consumer`, `categories.library_unknown` (not in the instance's last library listing; scan again), `categories.library_overlap` (another category has it on this instance), `validation.too_many`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn set_role_libraries(
    admin: AdminSession,
    State(state): State<AppState>,
    Path((category, id)): Path<(String, i64)>,
    Json(body): Json<SetRoleLibraries>,
) -> Result<Json<CategorySettings>, ApiError> {
    let category = parse_category(&category)?;
    let role = load_role(&state, category, id).await?;
    if role.kind != RoleKind::Consume {
        return Err(ApiError::Validation("categories.not_a_consumer"));
    }
    if body.library_ids.len() > MAX_ROLE_LIBRARIES {
        return Err(ApiError::Validation("validation.too_many"));
    }
    let listed: Vec<_> = state
        .db
        .service_libraries(role.service_instance_id)
        .await?
        .into_iter()
        .map(|l| l.library)
        .collect();
    let mut others = Vec::new();
    for other in state.db.list_roles(None).await? {
        if other.kind == RoleKind::Consume
            && other.service_instance_id == role.service_instance_id
            && other.id != role.id
        {
            others.push((other.category, other.consume_settings()?));
        }
    }
    let other_roles: Vec<RoleLibraries<'_>> = others
        .iter()
        .map(|(category, settings)| RoleLibraries {
            category: *category,
            libraries: &settings.libraries,
        })
        .collect();
    let current = role.consume_settings()?;
    let libraries = validate_mapping(
        category,
        &body.library_ids,
        &listed,
        &current.libraries,
        &other_roles,
    )
    .map_err(|e| ApiError::Validation(e.code()))?;
    let settings = ConsumeSettings { libraries };
    // Checked again inside the write transaction (concurrent saves).
    state
        .db
        .set_consume_libraries(id, &settings, state.clock.now())
        .await?
        .ok_or(ApiError::NotFound("categories.role_not_found"))?;
    audit(
        &state,
        &admin,
        "categories.libraries",
        category,
        &json!({
            "roleId": id,
            "serviceId": role.service_instance_id,
            "libraries": settings.libraries.iter().map(|l| &l.id).collect::<Vec<_>>(),
        }),
    )
    .await?;
    // Inventory rows written before libraries were recorded count for a
    // mapped role only after the next sync: run one now.
    enqueue_now(&state, JobKind::InventorySync).await?;
    super::service_accounts::accounts_changed(&state).await?;
    Ok(Json(settings_of(&state, category).await?))
}
