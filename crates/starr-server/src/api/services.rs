//! Service instances: `/api/v1/services`, `/api/v1/service-types` (admins).
//!
//! The web side seals API keys with the public recipient only and never
//! returns or opens them (ADR 0003). Checks run as `services.check` jobs in
//! the worker; these handlers only enqueue them.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::adapters::{AdapterDescriptor, Capability, SecretClass};
use starr_core::categories::{RoleKind, SourceSettings};
use starr_core::jobs::{JobKind, Trigger};
use starr_db::audit::{ActorKind, AuditEntry};
use starr_db::jobs::NewJob;
use starr_db::services::{NewServiceInstance, ServiceInstance, ServiceUpdate, StoredCredential};
use starr_worker::adapters;
use url::Url;
use utoipa::ToSchema;

use super::common::{self, CategoryDto, ConfirmRequired, Effect};
use super::error::{ApiError, ErrorBody};
use crate::AppState;
use crate::auth::AdminSession;

const NAME_MAX_CHARS: usize = 64;
// TMDB read access tokens are JWTs of a few hundred characters.
const API_KEY_MAX_CHARS: usize = 1024;
const BASE_URL_MAX_CHARS: usize = 2048;

/// Whether Starr only checks the connection (`probe`) or acts on the service.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ServiceMode {
    Probe,
    Live,
}

/// A connected service. The API key is never returned.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    id: i64,
    /// Adapter type id, e.g. `radarr`.
    adapter_type: String,
    name: String,
    base_url: String,
    /// Address members reach the service's web client at, for "open in"
    /// links (Jellyfin, Emby); absent: links use `baseUrl`.
    public_url: Option<String>,
    mode: ServiceMode,
    /// Version reported by the service at the last check that reached it.
    detected_version: Option<String>,
    /// Capabilities offered, from the last successful check.
    capabilities: Vec<String>,
    /// Absent until the first check finished.
    last_check_at: Option<DateTime<Utc>>,
    /// Error code of the last check (`credentials_invalid`, `transient`,
    /// `throttled`, `not_found`, `rejected`, `version_unsupported`,
    /// `unknown`, `credentials_unreadable`, `adapter_unknown`); absent when
    /// it succeeded.
    last_error: Option<String>,
    /// Sources: since when writes are paused. While paused, Starr adds,
    /// monitors and searches nothing there; approved requests wait (held)
    /// until the writes are resumed. Absent while writes go out.
    writes_paused_at: Option<DateTime<Utc>>,
    /// Username of the admin who paused the writes.
    writes_paused_by: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<ServiceInstance> for Service {
    type Error = ApiError;

    fn try_from(s: ServiceInstance) -> Result<Self, ApiError> {
        let mode = match s.mode.as_str() {
            "probe" => ServiceMode::Probe,
            "live" => ServiceMode::Live,
            other => {
                tracing::error!(mode = other, service = s.id, "unknown service mode");
                return Err(ApiError::Internal);
            }
        };
        Ok(Self {
            id: s.id,
            adapter_type: s.adapter_type,
            name: s.name,
            base_url: s.base_url,
            public_url: s.public_url,
            mode,
            detected_version: s.detected_version,
            capabilities: s.capabilities,
            last_check_at: s.last_check_at,
            last_error: s.last_error,
            writes_paused_at: s.writes_paused_at,
            writes_paused_by: s.writes_paused_by,
            created_at: s.created_at,
            updated_at: s.updated_at,
        })
    }
}

/// A service with the check job that was queued for it.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceWithCheck {
    service: Service,
    /// The queued `services.check` job; follow it via `/api/v1/runs?jobId=`.
    /// Absent if nothing about the connection changed.
    job_id: Option<i64>,
}

/// Pause or resume writes to a source.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetWritesPaused {
    paused: bool,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CheckQueued {
    job_id: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateService {
    /// Adapter type id from `/api/v1/service-types`.
    adapter_type: String,
    name: String,
    /// e.g. `http://radarr:7878` (including a URL base, if configured).
    base_url: String,
    /// API key or token; omitted (or empty) for types that take none
    /// (`ServiceType.needsCredential`, e.g. `MusicBrainz`).
    #[serde(default)]
    #[schema(value_type = Option<String>, format = Password)]
    api_key: Option<SecretString>,
    /// Types that take a login (`ServiceType.credential` = `login`,
    /// Navidrome): the account's username, with `password`.
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    #[schema(value_type = Option<String>, format = Password)]
    password: Option<SecretString>,
    /// Only for services with libraries (Jellyfin, Emby, Navidrome): the
    /// address members use, if it differs from `baseUrl`.
    #[serde(default)]
    public_url: Option<String>,
}

/// Fields to change; omitted fields stay as they are.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateService {
    name: Option<String>,
    base_url: Option<String>,
    #[schema(value_type = Option<String>, format = Password)]
    api_key: Option<SecretString>,
    /// A new login (types that take one): username and password together.
    username: Option<String>,
    #[schema(value_type = Option<String>, format = Password)]
    password: Option<SecretString>,
    /// `null` or `""` removes the public URL; omitted keeps it.
    // Omitted, null and a value mean three different things here.
    #[allow(clippy::option_option)]
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<String>, nullable)]
    public_url: Option<Option<String>>,
    /// The admin agreed to the `effects` a previous attempt answered with
    /// (409 `services.confirm_required`).
    #[serde(default)]
    confirm: bool,
}

/// Distinguish an explicit `null` (`Some(None)`) from an omitted field
/// (`None`, via `#[serde(default)]`).
#[allow(clippy::option_option)]
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

/// Versions an adapter was tested against.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestedVersions {
    /// Inclusive.
    min: String,
    /// Exclusive.
    max_exclusive: String,
}

/// An adapter type this Starr build can connect.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceType {
    type_id: &'static str,
    display_name: &'static str,
    capabilities: Vec<&'static str>,
    /// Categories the adapter can serve (e.g. Radarr: movies).
    categories: Vec<CategoryDto>,
    tested_versions: TestedVersions,
    /// Whether the type takes an API key or token (`MusicBrainz` takes none).
    needs_credential: bool,
    /// What the credential is: an API key or token, an account's username
    /// and password (Navidrome has no API key), or none.
    credential: CredentialKind,
    /// The URL most operators enter (public services such as TMDB and
    /// `MusicBrainz`); absent for self-hosted services.
    default_base_url: Option<&'static str>,
}

impl From<&AdapterDescriptor> for ServiceType {
    fn from(d: &AdapterDescriptor) -> Self {
        Self {
            type_id: d.type_id,
            display_name: d.display_name,
            capabilities: d.capabilities.iter().map(|c| c.as_str()).collect(),
            categories: adapters::categories(d.type_id)
                .iter()
                .map(|c| CategoryDto::from(*c))
                .collect(),
            tested_versions: TestedVersions {
                min: d.tested_versions.min.to_string(),
                max_exclusive: d.tested_versions.max_exclusive.to_string(),
            },
            needs_credential: adapters::needs_credential(d.type_id),
            credential: CredentialKind::of(d.type_id),
            default_base_url: adapters::default_base_url(d.type_id),
        }
    }
}

/// What credential an adapter type takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    ApiKey,
    Login,
    None,
}

impl CredentialKind {
    fn of(type_id: &str) -> Self {
        if adapters::takes_login(type_id) {
            Self::Login
        } else if adapters::needs_credential(type_id) {
            Self::ApiKey
        } else {
            Self::None
        }
    }
}

/// A login's username: trimmed, not empty, no control characters.
fn validate_username(raw: &str) -> Result<String, ApiError> {
    let name = raw.trim();
    let len = name.chars().count();
    if len == 0 || len > NAME_MAX_CHARS * 4 || name.chars().any(char::is_control) {
        return Err(ApiError::Validation("validation.login"));
    }
    Ok(name.to_owned())
}

/// A login's password: kept exactly as typed (Navidrome has no rules), not
/// empty, no control characters.
fn validate_password(raw: &SecretString) -> Result<(), ApiError> {
    let password = raw.expose_secret();
    if password.is_empty()
        || password.len() > API_KEY_MAX_CHARS
        || password.chars().any(char::is_control)
    {
        return Err(ApiError::Validation("validation.login"));
    }
    Ok(())
}

/// What a create or update says about the credential.
#[derive(Clone, Copy)]
struct GivenCredential<'a> {
    api_key: Option<&'a SecretString>,
    username: Option<&'a str>,
    password: Option<&'a SecretString>,
}

impl GivenCredential<'_> {
    fn any(&self) -> bool {
        self.api_key.is_some() || self.username.is_some() || self.password.is_some()
    }
}

/// The credential to store for a type that takes a login: username and
/// password together, never an API key.
fn login_for(given: GivenCredential<'_>) -> Result<SecretString, ApiError> {
    if given.api_key.is_some() {
        return Err(ApiError::Validation("validation.api_key"));
    }
    let (Some(username), Some(password)) = (given.username, given.password) else {
        return Err(ApiError::Validation("validation.login"));
    };
    let username = validate_username(username)?;
    validate_password(password)?;
    Ok(adapters::login_credential(&username, password))
}

fn validate_name(raw: &str) -> Result<&str, ApiError> {
    let name = raw.trim();
    let len = name.chars().count();
    if len == 0 || len > NAME_MAX_CHARS || name.chars().any(char::is_control) {
        return Err(ApiError::Validation("validation.service_name"));
    }
    Ok(name)
}

/// Absolute http(s) URL with a host, no credentials, query or fragment.
/// Stored without trailing slash.
fn validate_base_url(raw: &str) -> Result<String, ApiError> {
    let invalid = ApiError::Validation("validation.base_url");
    let raw = raw.trim();
    if raw.len() > BASE_URL_MAX_CHARS {
        return Err(invalid);
    }
    let url = Url::parse(raw).map_err(|_| invalid)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid);
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// A public URL (validated like a base URL) for adapters that build links;
/// empty means none.
fn validate_public_url(adapter_type: &str, raw: &str) -> Result<Option<String>, ApiError> {
    if raw.trim().is_empty() {
        return Ok(None);
    }
    if !adapters::has_library(adapter_type) {
        return Err(ApiError::Validation("validation.public_url"));
    }
    validate_base_url(raw)
        .map(Some)
        .map_err(|_| ApiError::Validation("validation.public_url"))
}

/// The stored credential was entered for the current server: a new URL on
/// another origin needs it typed again (`credential_given`), so it is never
/// handed to a host it was not meant for. Types without a credential pass.
fn keep_credential_on_its_origin(
    current: &ServiceInstance,
    new_url: Option<&str>,
    credential_given: bool,
) -> Result<(), ApiError> {
    match new_url {
        Some(url)
            if !credential_given
                && adapters::needs_credential(&current.adapter_type)
                && !same_origin(url, &current.base_url) =>
        {
            Err(ApiError::Validation(
                "validation.credential_required_for_new_origin",
            ))
        }
        _ => Ok(()),
    }
}

/// Whether two validated base URLs share scheme, host and port.
fn same_origin(a: &str, b: &str) -> bool {
    match (Url::parse(a), Url::parse(b)) {
        (Ok(a), Ok(b)) => a.origin() == b.origin(),
        _ => false,
    }
}

/// TMDB paths start at `/3/`; a base URL with a path (e.g. `…/3`) would
/// double it and fail with a confusing "not found".
fn check_tmdb_base_url(adapter_type: &str, base_url: &str) -> Result<(), ApiError> {
    let has_path = Url::parse(base_url).map_or(true, |u| u.path() != "/");
    if adapter_type == starr_adapter_tmdb::DESCRIPTOR.type_id && has_path {
        return Err(ApiError::Validation("validation.tmdb_base_url"));
    }
    Ok(())
}

/// Printable ASCII without spaces: what HTTP headers can carry. TMDB takes
/// only the API Read Access Token (a bearer token); its 32-hex v3 "API key"
/// works only as a query parameter, which Starr never sends credentials in,
/// so it is refused with a pointer to the right value.
fn validate_api_key(raw: &SecretString, adapter_type: &str) -> Result<SecretString, ApiError> {
    let key = raw.expose_secret().trim();
    if key.is_empty() || key.len() > API_KEY_MAX_CHARS || !key.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(ApiError::Validation("validation.api_key"));
    }
    if adapter_type == starr_adapter_tmdb::DESCRIPTOR.type_id
        && starr_adapter_tmdb::is_hex_api_key(key)
    {
        return Err(ApiError::Validation("validation.tmdb_read_token"));
    }
    Ok(SecretString::from(key.to_owned()))
}

/// The credential to store for a new instance: the validated key, or an
/// empty value for types that take none (a key given there is refused, so
/// nothing secret is ever sent to a service that does not ask for it).
fn credential_for(
    adapter_type: &str,
    given: GivenCredential<'_>,
) -> Result<SecretString, ApiError> {
    if adapters::takes_login(adapter_type) {
        return login_for(given);
    }
    if given.username.is_some() || given.password.is_some() {
        return Err(ApiError::Validation("validation.login"));
    }
    let given = given
        .api_key
        .filter(|k| !k.expose_secret().trim().is_empty());
    match (adapters::needs_credential(adapter_type), given) {
        (true, Some(key)) => validate_api_key(key, adapter_type),
        (true, None) | (false, Some(_)) => Err(ApiError::Validation("validation.api_key")),
        (false, None) => Ok(SecretString::from(String::new())),
    }
}

/// Check a `StashDB` key before it is stored (spec 0007 Q14, "checked on
/// save"): it must be accepted and its account may only read. The web
/// process may call the catalog: the key is a catalog read token (ADR 0003
/// amendment 1). Nothing of the answer is logged but its error code.
async fn check_stashdb_key(
    state: &AppState,
    base_url: &str,
    api_key: &SecretString,
) -> Result<(), ApiError> {
    let url = Url::parse(base_url).map_err(|_| ApiError::Validation("validation.base_url"))?;
    let key = SecretString::from(api_key.expose_secret().to_owned());
    match state.catalog.check_stashdb_key(url, key).await {
        Ok(check) if check.read_only() => Ok(()),
        Ok(_) => Err(ApiError::Validation("validation.stashdb_not_read_only")),
        Err(starr_core::adapters::AdapterError::CredentialsInvalid) => {
            Err(ApiError::Validation("validation.api_key_refused"))
        }
        Err(err) => {
            tracing::warn!(code = err.code(), "checking a StashDB key failed");
            Err(ApiError::Unavailable("services.check_unavailable"))
        }
    }
}

/// One `StashDB` per Starr: its budget is per host (spec 0007 Q1), so a
/// second instance would double the requests Starr sends there.
async fn refuse_second_stashdb(state: &AppState, adapter_type: &str) -> Result<(), ApiError> {
    if adapter_type == starr_adapter_stashdb::DESCRIPTOR.type_id
        && state
            .db
            .list_services()
            .await?
            .iter()
            .any(|s| s.adapter_type == adapter_type)
    {
        return Err(ApiError::Conflict("services.stashdb_exists"));
    }
    Ok(())
}

/// Seal `api_key` with the key of the adapter's secret class (ADR 0003
/// amendment 1): service credentials for the worker's recipient, catalog
/// tokens for the web key.
async fn seal(
    state: &AppState,
    descriptor: &AdapterDescriptor,
    api_key: &SecretString,
) -> Result<StoredCredential, ApiError> {
    let failed = |err: starr_secrets::SecretsError| {
        tracing::error!(error = %err, "sealing a service credential failed");
        ApiError::Internal
    };
    let class = descriptor.secret_class;
    let (recipient, ciphertext) = match class {
        SecretClass::ServiceCredential => state
            .credential_sealer()
            .await?
            .seal(api_key)
            .map_err(failed)?
            .into_parts(),
        SecretClass::CatalogReadToken => state
            .catalog
            .sealer()
            .seal(api_key)
            .map_err(failed)?
            .into_parts(),
    };
    Ok(StoredCredential {
        ciphertext,
        recipient,
        class,
    })
}

/// [`seal`] for a stored instance's adapter type.
async fn seal_for_type(
    state: &AppState,
    adapter_type: &str,
    api_key: &SecretString,
) -> Result<StoredCredential, ApiError> {
    let descriptor = adapters::descriptor(adapter_type).ok_or_else(|| {
        tracing::error!(adapter = %adapter_type, "service of an unknown adapter type");
        ApiError::Validation("validation.adapter_type")
    })?;
    seal(state, descriptor, api_key).await
}

/// Queue a check of one instance (deduplicated per instance) and wake the
/// worker.
async fn enqueue_check(state: &AppState, id: i64) -> Result<i64, ApiError> {
    let now = Utc::now();
    let kind = JobKind::ServicesCheck;
    let key = format!("{}:{id}", kind.name());
    let payload = json!({ "instanceId": id }).to_string();
    let job_id = state
        .db
        .enqueue_job(
            &NewJob {
                kind: kind.name(),
                payload: &payload,
                unique_key: Some(&key),
                trigger: Trigger::Manual,
                run_after: now,
                max_attempts: kind.spec().max_attempts,
            },
            now,
        )
        .await?;
    state.wake.notify_one();
    Ok(job_id)
}

async fn audit(
    state: &AppState,
    admin: &AdminSession,
    action: &str,
    target: &str,
    details: &serde_json::Value,
) -> Result<(), ApiError> {
    state
        .db
        .audit(
            &AuditEntry {
                actor_kind: ActorKind::Admin,
                actor_name: Some(&admin.0.subject.username),
                action,
                target: Some(target),
                details: Some(&details.to_string()),
            },
            state.clock.now(),
        )
        .await?;
    Ok(())
}

async fn load(state: &AppState, id: i64) -> Result<ServiceInstance, ApiError> {
    state
        .db
        .get_service(id)
        .await?
        .ok_or(ApiError::NotFound("services.not_found"))
}

/// Adapter types this Starr build can connect.
#[utoipa::path(get, path = "/api/v1/service-types", tag = "services",
    responses(
        (status = 200, body = Vec<ServiceType>),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
    ))]
pub async fn list_service_types(_admin: AdminSession) -> Json<Vec<ServiceType>> {
    Json(adapters::descriptors().map(ServiceType::from).collect())
}

/// All service instances, by name.
#[utoipa::path(get, path = "/api/v1/services", tag = "services",
    responses(
        (status = 200, body = Vec<Service>),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_services(
    _admin: AdminSession,
    State(state): State<AppState>,
) -> Result<Json<Vec<Service>>, ApiError> {
    let services = state.db.list_services().await?;
    Ok(Json(
        services
            .into_iter()
            .map(Service::try_from)
            .collect::<Result<_, _>>()?,
    ))
}

/// One service instance.
#[utoipa::path(get, path = "/api/v1/services/{id}", tag = "services",
    params(("id" = i64, Path, description = "Service id")),
    responses(
        (status = 200, body = Service),
        (status = 404, body = ErrorBody, description = "`services.not_found`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn get_service(
    _admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Service>, ApiError> {
    Ok(Json(load(&state, id).await?.try_into()?))
}

/// Connect a service: stores it with the API key sealed and queues a check.
#[utoipa::path(post, path = "/api/v1/services", tag = "services",
    request_body = CreateService,
    responses(
        (status = 202, body = ServiceWithCheck, description = "Stored; the check runs in the background"),
        (status = 409, body = ErrorBody, description = "`services.name_taken`, `services.musicbrainz_exists` (one MusicBrainz per Starr: its budget is per IP), `services.stashdb_exists` (one StashDB per Starr: its budget is per host)"),
        (status = 422, body = ErrorBody, description = "`validation.adapter_type`, `validation.service_name`, `validation.base_url`, `validation.api_key` (missing, invalid, or given for a type that takes none), `validation.tmdb_read_token` (a TMDB v3 API key instead of the read access token), `validation.tmdb_base_url` (a TMDB URL with a path), `validation.public_url` (invalid, or the type builds no links), `validation.api_key_refused` (StashDB refused the key), `validation.stashdb_not_read_only` (the StashDB account can do more than read)"),
        (status = 503, body = ErrorBody, description = "`services.secrets_unavailable` (worker has not initialised the key yet), `services.check_unavailable` (StashDB could not be asked to check the key)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn create_service(
    admin: AdminSession,
    State(state): State<AppState>,
    Json(body): Json<CreateService>,
) -> Result<(StatusCode, Json<ServiceWithCheck>), ApiError> {
    let descriptor = adapters::descriptor(&body.adapter_type)
        .ok_or(ApiError::Validation("validation.adapter_type"))?;
    let name = validate_name(&body.name)?;
    let base_url = validate_base_url(&body.base_url)?;
    check_tmdb_base_url(descriptor.type_id, &base_url)?;
    let api_key = credential_for(
        descriptor.type_id,
        GivenCredential {
            api_key: body.api_key.as_ref(),
            username: body.username.as_deref(),
            password: body.password.as_ref(),
        },
    )?;
    // MusicBrainz's budget is per source IP: a second instance would double
    // the requests Starr sends (spec 0006 Q14).
    if descriptor.type_id == starr_adapter_musicbrainz::DESCRIPTOR.type_id
        && state
            .db
            .list_services()
            .await?
            .iter()
            .any(|s| s.adapter_type == descriptor.type_id)
    {
        return Err(ApiError::Conflict("services.musicbrainz_exists"));
    }
    refuse_second_stashdb(&state, descriptor.type_id).await?;
    if descriptor.type_id == starr_adapter_stashdb::DESCRIPTOR.type_id {
        check_stashdb_key(&state, &base_url, &api_key).await?;
    }
    let public_url = body
        .public_url
        .as_deref()
        .map(|url| validate_public_url(descriptor.type_id, url))
        .transpose()?
        .flatten();
    let credential = seal(&state, descriptor, &api_key).await?;

    let created = state
        .db
        .create_service(
            &NewServiceInstance {
                adapter_type: descriptor.type_id,
                name,
                base_url: &base_url,
                public_url: public_url.as_deref(),
                credential: &credential,
            },
            state.clock.now(),
        )
        .await?;
    audit(
        &state,
        &admin,
        "services.create",
        &created.name,
        &json!({ "id": created.id, "adapterType": created.adapter_type, "baseUrl": created.base_url, "publicUrl": created.public_url }),
    )
    .await?;
    let job_id = enqueue_check(&state, created.id).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(ServiceWithCheck {
            service: created.try_into()?,
            job_id: Some(job_id),
        }),
    ))
}

/// What a new address of instance `id` does to the categories it is the
/// source of (spec 0008 ADM-01): their settings stay and are checked
/// against the lists read from there. `None` when it touches none.
async fn confirm_new_address(
    state: &AppState,
    id: i64,
) -> Result<Option<ConfirmRequired>, ApiError> {
    let mut effects = Vec::new();
    for role in state.db.list_roles(None).await? {
        if role.kind == RoleKind::Source
            && role.service_instance_id == id
            && role.source_settings()? != SourceSettings::default()
        {
            effects.push(Effect::SourceSettingsRechecked {
                category: role.category.into(),
            });
        }
    }
    Ok((!effects.is_empty()).then_some(ConfirmRequired {
        code: "services.confirm_required",
        effects,
    }))
}

/// Read the lists of source `id` again after its address changed, so the
/// settings of its categories are checked against the new server.
async fn recheck_source_settings(state: &AppState, id: i64) -> Result<(), ApiError> {
    let is_source = state
        .db
        .list_roles(None)
        .await?
        .iter()
        .any(|r| r.kind == RoleKind::Source && r.service_instance_id == id);
    if is_source {
        super::catalog::enqueue_source_lists(state, id).await?;
    }
    Ok(())
}

/// Rename a service or change its URL, API key or public URL. A changed URL
/// or key clears the last check result and queues a new check. Changing the
/// URL or the credential needs a fresh elevation (`POST /api/v1/auth/elevate`);
/// a URL on another origin (scheme, host or port) also needs the credential
/// again in the same request, so a stored key never follows the address to
/// another server. A new URL of
/// a source keeps the settings its categories picked from it; the lists are
/// read again from there, and what that server does not offer is cleared
/// (spec 0008 ADM-01). Because that can close a category's requests, the
/// first attempt answers 409 with the `effects` and changes nothing; send
/// it again with `confirm`.
#[utoipa::path(patch, path = "/api/v1/services/{id}", tag = "services",
    params(("id" = i64, Path, description = "Service id")),
    request_body = UpdateService,
    responses(
        (status = 200, body = ServiceWithCheck),
        (status = 404, body = ErrorBody, description = "`services.not_found`"),
        (status = 409, body = ConfirmRequired, description = "`services.confirm_required` (with `effects`; nothing changed), `services.name_taken` (code only)"),
        (status = 422, body = ErrorBody, description = "`validation.service_name`, `validation.base_url`, `validation.api_key`, `validation.public_url`, `validation.credential_required_for_new_origin` (a URL on another origin without the API key or login), `validation.api_key_refused`, `validation.stashdb_not_read_only` (a new StashDB key is checked before it is stored)"),
        (status = 503, body = ErrorBody, description = "`services.secrets_unavailable`, `services.check_unavailable`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin), `auth.elevation_required` (URL or credential change without a fresh re-authentication)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn update_service(
    admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateService>,
) -> Result<Result<Json<ServiceWithCheck>, ConfirmRequired>, ApiError> {
    let name = body.name.as_deref().map(validate_name).transpose()?;
    let base_url = body
        .base_url
        .as_deref()
        .map(validate_base_url)
        .transpose()?;
    let current = load(&state, id).await?;
    let given = GivenCredential {
        api_key: body.api_key.as_ref(),
        username: body.username.as_deref(),
        password: body.password.as_ref(),
    };
    let api_key = if given.any() {
        Some(credential_for(&current.adapter_type, given)?)
    } else {
        None
    };
    if let Some(url) = &base_url {
        check_tmdb_base_url(&current.adapter_type, url)?;
    }
    let public_url = match &body.public_url {
        None => None,
        Some(None) => Some(None),
        Some(Some(url)) => Some(validate_public_url(&current.adapter_type, url)?),
    }
    .filter(|url| *url != current.public_url);
    // Only real changes count: re-sending the same URL keeps the check result.
    let base_url = base_url.filter(|url| *url != current.base_url);
    keep_credential_on_its_origin(&current, base_url.as_deref(), api_key.is_some())?;
    if base_url.is_some() || api_key.is_some() {
        common::require_elevation(&state, &admin.0)?;
    }
    if base_url.is_some()
        && !body.confirm
        && let Some(confirm) = confirm_new_address(&state, id).await?
    {
        return Ok(Err(confirm));
    }
    if let Some(key) = &api_key
        && current.adapter_type == starr_adapter_stashdb::DESCRIPTOR.type_id
    {
        let url = base_url.as_deref().unwrap_or(&current.base_url);
        check_stashdb_key(&state, url, key).await?;
    }
    let credential = match &api_key {
        Some(key) => Some(seal_for_type(&state, &current.adapter_type, key).await?),
        None => None,
    };

    let mut fields = Vec::new();
    if name.is_some_and(|n| n != current.name) {
        fields.push("name");
    }
    if base_url.is_some() {
        fields.push("baseUrl");
    }
    if credential.is_some() {
        fields.push("apiKey");
    }
    if public_url.is_some() {
        fields.push("publicUrl");
    }
    let updated = state
        .db
        .update_service(
            id,
            &ServiceUpdate {
                name,
                base_url: base_url.as_deref(),
                public_url: public_url.as_ref().map(Option::as_deref),
                credential: credential.as_ref(),
            },
            state.clock.now(),
        )
        .await?
        .ok_or(ApiError::NotFound("services.not_found"))?;
    audit(
        &state,
        &admin,
        "services.update",
        &updated.name,
        &json!({ "id": id, "fields": fields, "previousName": current.name }),
    )
    .await?;
    let job_id = if base_url.is_some() || credential.is_some() {
        Some(enqueue_check(&state, id).await?)
    } else {
        None
    };
    // Saved already: a failure to queue the read is logged, not answered
    // (the next manual or scheduled read checks the settings too).
    if base_url.is_some()
        && let Err(err) = recheck_source_settings(&state, id).await
    {
        tracing::warn!(service = id, error = ?err, "queueing the source lists read failed");
    }
    Ok(Ok(Json(ServiceWithCheck {
        service: updated.try_into()?,
        job_id,
    })))
}

/// Remove a service from Starr. The service itself is not touched.
#[utoipa::path(delete, path = "/api/v1/services/{id}", tag = "services",
    params(("id" = i64, Path, description = "Service id")),
    responses(
        (status = 204, description = "Removed"),
        (status = 404, body = ErrorBody, description = "`services.not_found`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn delete_service(
    admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let current = load(&state, id).await?;
    if !state.db.delete_service(id, state.clock.now()).await? {
        return Err(ApiError::NotFound("services.not_found"));
    }
    audit(
        &state,
        &admin,
        "services.delete",
        &current.name,
        &json!({ "id": id, "adapterType": current.adapter_type, "baseUrl": current.base_url }),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Check a service now (connection, credentials, version).
#[utoipa::path(post, path = "/api/v1/services/{id}/check", tag = "services",
    params(("id" = i64, Path, description = "Service id")),
    responses(
        (status = 202, body = CheckQueued, description = "Queued; if a check is already pending, that job"),
        (status = 404, body = ErrorBody, description = "`services.not_found`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn check_service(
    admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<(StatusCode, Json<CheckQueued>), ApiError> {
    let current = load(&state, id).await?;
    let job_id = enqueue_check(&state, id).await?;
    audit(
        &state,
        &admin,
        "services.check",
        &current.name,
        &json!({ "id": id, "jobId": job_id }),
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(CheckQueued { job_id })))
}

/// Pause or resume writes to a source (Radarr, Sonarr), e.g. during its
/// maintenance or to try Starr against a production instance. While paused,
/// approvals still work, but nothing is added, monitored or searched there:
/// the acquisitions wait as held. Reading (sync, lists) goes on. A write
/// already on its way to the source when the pause lands still completes.
/// Resuming queues `requests.submit`, which sends the held acquisitions out.
/// Only a change is audit-logged; repeating the current state is a no-op.
#[utoipa::path(put, path = "/api/v1/services/{id}/writes-paused", tag = "services",
    params(("id" = i64, Path, description = "Service id")),
    request_body = SetWritesPaused,
    responses(
        (status = 200, body = Service),
        (status = 404, body = ErrorBody, description = "`services.not_found`"),
        (status = 422, body = ErrorBody, description = "`sources.not_a_source` (the service has no writes to pause)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn set_writes_paused(
    admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<SetWritesPaused>,
) -> Result<Json<Service>, ApiError> {
    let current = load(&state, id).await?;
    let is_source = adapters::descriptor(&current.adapter_type)
        .is_some_and(|d| d.capabilities.contains(&Capability::Requests));
    if !is_source {
        return Err(ApiError::Validation("sources.not_a_source"));
    }
    let paused_by = body.paused.then_some(admin.0.subject.username.as_str());
    let (updated, changed) = state
        .db
        .set_writes_paused(id, paused_by, state.clock.now())
        .await?
        .ok_or(ApiError::NotFound("services.not_found"))?;
    if changed {
        let (action, details) = if body.paused {
            ("services.writes_paused", json!({ "id": id }))
        } else {
            (
                "services.writes_resumed",
                json!({ "id": id, "pausedSince": current.writes_paused_at, "pausedBy": current.writes_paused_by }),
            )
        };
        audit(&state, &admin, action, &updated.name, &details).await?;
        if !body.paused {
            super::requests::enqueue_submit(&state).await?;
        }
    }
    Ok(Json(updated.try_into()?))
}

/// How many held titles the resume confirmation names.
const HELD_PREVIEW: u32 = 5;

/// What resuming a paused source sends out.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HeldAcquisitions {
    /// Approved acquisitions held there; 0 while writes go out.
    pub(crate) total: u32,
    /// The first five of them, in the order they will be sent.
    preview: Vec<HeldTitle>,
    /// Stops held there (stop monitoring what Starr turned on, because
    /// nobody wants the title any more, spec 0004); 0 while writes go out.
    pub(crate) stop_total: u32,
    /// The first five of them, in the order they will be sent.
    stops: Vec<HeldTitle>,
}

/// One held acquisition: a movie, a series with its season, or an artist
/// (`title`) with the album.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HeldTitle {
    title: String,
    year: Option<i64>,
    season: Option<u32>,
    /// Music: the album's title.
    album: Option<String>,
    /// In the private category while the admin's session has not unlocked
    /// it (spec 0007 Q9): title, year, season and album are left out.
    redacted: bool,
}

/// What resuming the writes of a source would send out: how many approved
/// acquisitions and stops (stop monitoring) are held there and the first
/// five of each, so the admin confirms with the titles in view. Empty while
/// writes go out.
#[utoipa::path(get, path = "/api/v1/services/{id}/held", tag = "services",
    params(("id" = i64, Path, description = "Service id")),
    responses(
        (status = 200, body = HeldAcquisitions),
        (status = 404, body = ErrorBody, description = "`services.not_found`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn held(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<HeldAcquisitions>, ApiError> {
    load(&state, id).await?;
    let adult = super::privacy::adult_open(&state, &session).await?;
    Ok(Json(held_acquisitions(&state, id, adult).await?))
}

/// What resuming the writes of source `id` sends out; private titles
/// neutral unless `adult` (the session unlocked the private category).
pub(crate) async fn held_acquisitions(
    state: &AppState,
    id: i64,
    adult: bool,
) -> Result<HeldAcquisitions, ApiError> {
    let held = state.db.held_at(id, HELD_PREVIEW).await?;
    let titles = |titles: Vec<starr_db::requests::HeldTitle>| {
        titles
            .into_iter()
            .map(|mut t| {
                let redacted = super::privacy::neutral(t.category, adult);
                if redacted {
                    super::privacy::redact_held(&mut t);
                }
                HeldTitle {
                    title: t.title,
                    year: t.year,
                    season: t.season,
                    album: t.album,
                    redacted,
                }
            })
            .collect()
    };
    Ok(HeldAcquisitions {
        total: held.total,
        preview: titles(held.preview),
        stop_total: held.stops,
        stops: titles(held.stop_preview),
    })
}

impl HeldAcquisitions {
    /// Some title in it is shown neutrally.
    pub(crate) fn redacted(&self) -> bool {
        self.preview.iter().chain(&self.stops).any(|t| t.redacted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls_are_validated_and_normalised() {
        assert_eq!(
            validate_base_url(" http://radarr:7878/ ").unwrap(),
            "http://radarr:7878"
        );
        assert_eq!(
            validate_base_url("https://media.example.com/radarr/").unwrap(),
            "https://media.example.com/radarr"
        );
        for bad in [
            "radarr:7878",
            "ftp://radarr",
            "http://user:pw@radarr",
            "http://radarr/?apikey=x",
            "http://radarr/#x",
            "",
        ] {
            assert!(validate_base_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn names_and_keys_are_validated() {
        assert_eq!(validate_name("  Movies ").unwrap(), "Movies");
        assert!(validate_name("   ").is_err());
        assert!(validate_name(&"x".repeat(65)).is_err());
        assert!(validate_name("a\nb").is_err());
        let key = |s: &str| validate_api_key(&SecretString::from(s.to_owned()), "radarr");
        assert_eq!(key(" abc123 ").unwrap().expose_secret(), "abc123");
        assert!(key("").is_err());
        assert!(key("has space").is_err());
        assert!(key("ümlaut").is_err());
        let v3 = "0123456789abcdef0123456789ABCDEF";
        assert!(key(v3).is_ok());
        let tmdb = |s: &str| validate_api_key(&SecretString::from(s.to_owned()), "tmdb");
        assert_eq!(
            tmdb(v3).unwrap_err(),
            ApiError::Validation("validation.tmdb_read_token")
        );
        assert!(check_tmdb_base_url("tmdb", "https://api.themoviedb.org").is_ok());
        assert_eq!(
            check_tmdb_base_url("tmdb", "https://api.themoviedb.org/3"),
            Err(ApiError::Validation("validation.tmdb_base_url"))
        );
        assert!(check_tmdb_base_url("radarr", "http://radarr:7878/radarr").is_ok());
        assert_eq!(
            validate_public_url("jellyfin", " https://watch.example.com/ ").unwrap(),
            Some("https://watch.example.com".to_owned())
        );
        assert_eq!(validate_public_url("emby", "  ").unwrap(), None);
        assert_eq!(validate_public_url("radarr", "").unwrap(), None);
        for (kind, bad) in [
            ("jellyfin", "watch.example.com"),
            ("radarr", "https://r.example.com"),
        ] {
            assert_eq!(
                validate_public_url(kind, bad),
                Err(ApiError::Validation("validation.public_url")),
                "{kind} {bad}"
            );
        }
        let jwt = format!("eyJhbGciOiJIUzI1NiJ9.{}.sig", "x".repeat(300));
        assert!(tmdb(&jwt).is_ok());
        // MusicBrainz takes no key: none is stored, and one given is refused.
        let key = SecretString::from("abc".to_owned());
        let blank = SecretString::from(" ".to_owned());
        assert_eq!(
            credential_for("musicbrainz", api_key(None))
                .unwrap()
                .expose_secret(),
            ""
        );
        assert_eq!(
            credential_for("musicbrainz", api_key(Some(&blank)))
                .unwrap()
                .expose_secret(),
            ""
        );
        assert_eq!(
            credential_for("musicbrainz", api_key(Some(&key))).unwrap_err(),
            ApiError::Validation("validation.api_key")
        );
        assert_eq!(
            credential_for("radarr", api_key(None)).unwrap_err(),
            ApiError::Validation("validation.api_key")
        );
    }

    fn api_key(key: Option<&SecretString>) -> GivenCredential<'_> {
        GivenCredential {
            api_key: key,
            username: None,
            password: None,
        }
    }

    fn login<'a>(
        username: Option<&'a str>,
        password: Option<&'a SecretString>,
    ) -> GivenCredential<'a> {
        GivenCredential {
            api_key: None,
            username,
            password,
        }
    }

    #[test]
    fn navidrome_takes_a_login_and_nothing_else() {
        let password = SecretString::from(" pa ss ".to_owned());
        let stored = credential_for("navidrome", login(Some(" starr "), Some(&password))).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(stored.expose_secret()).unwrap();
        assert_eq!(parsed["username"], "starr", "trimmed");
        assert_eq!(parsed["password"], " pa ss ", "kept as typed");
        for (username, password) in [
            (None, Some(&password)),
            (Some("starr"), None),
            (Some("  "), Some(&password)),
            (Some("starr"), Some(&SecretString::from(String::new()))),
            (Some("st\narr"), Some(&password)),
        ] {
            assert_eq!(
                credential_for("navidrome", login(username, password)).unwrap_err(),
                ApiError::Validation("validation.login")
            );
        }
        let key = SecretString::from("key-0123456789".to_owned());
        assert_eq!(
            credential_for(
                "navidrome",
                GivenCredential {
                    api_key: Some(&key),
                    ..login(Some("starr"), Some(&password))
                }
            )
            .unwrap_err(),
            ApiError::Validation("validation.api_key")
        );
        // A login is no API key.
        assert_eq!(
            credential_for("radarr", login(Some("starr"), Some(&password))).unwrap_err(),
            ApiError::Validation("validation.login")
        );
    }
}
