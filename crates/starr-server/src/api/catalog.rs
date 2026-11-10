//! Catalog reference data, watch providers and the relevance preview of a
//! catalog instance (`/api/v1/services/{id}/catalog/...`), plus the
//! quality profiles and root folders of a source instance
//! (`/api/v1/services/{id}/source-lists`). Admins only.
//!
//! Catalog calls run in the web process with the web key (ADR 0003
//! amendment 1); source lists are read by the worker (`sources.lists`),
//! because source credentials never leave it.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use starr_core::adapters::AdapterError;
use starr_core::catalog::{
    Catalog, CatalogConfiguration, CatalogEntry, ListKind, ListQuery, MediaKind, ReleaseType,
    Relevance, SecondaryType,
};
use starr_core::jobs::{JobKind, Trigger};
use starr_db::jobs::NewJob;
use starr_worker::catalog::{AnyCatalog, CatalogOpenError};
use utoipa::{IntoParams, ToSchema};

use super::error::{ApiError, ErrorBody};
use crate::AppState;
use crate::auth::AdminSession;

/// Language for preview titles (D5: English in v1).
const PREVIEW_LANGUAGE: &str = "en-US";

/// What a title is: a movie or series (TMDB), or an artist or album
/// (`MusicBrainz`, spec 0006). Requests exist for movies and series only
/// until music has its request flow (spec 0006 slice 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = MediaKind)]
pub enum MediaKindDto {
    Movie,
    Series,
    Artist,
    Album,
    /// A scene of the private category (spec 0007 slice 4): only in
    /// requests and acquisitions, behind the privacy gate; global Discover,
    /// search and title routes never name one.
    Scene,
}

impl From<MediaKindDto> for MediaKind {
    fn from(k: MediaKindDto) -> Self {
        match k {
            MediaKindDto::Movie => Self::Movie,
            MediaKindDto::Series => Self::Series,
            MediaKindDto::Artist => Self::Artist,
            MediaKindDto::Album => Self::Album,
            MediaKindDto::Scene => Self::Scene,
        }
    }
}

impl MediaKindDto {
    /// The API name of `kind` in catalog answers; `None` for the adult
    /// kinds, which only the private area's own routes serve (spec 0007
    /// Q11).
    pub(crate) const fn of(kind: MediaKind) -> Option<Self> {
        match kind {
            MediaKind::Movie => Some(Self::Movie),
            MediaKind::Series => Some(Self::Series),
            MediaKind::Artist => Some(Self::Artist),
            MediaKind::Album => Some(Self::Album),
            MediaKind::Scene | MediaKind::Performer | MediaKind::Studio => None,
        }
    }

    /// The kind as the admin catalog tools take it: scenes only through
    /// the private area (spec 0007 Q11), so `scene` is refused here.
    fn tool_kind(self) -> Result<MediaKind, ApiError> {
        match self {
            Self::Scene => Err(ApiError::Validation("validation.kind")),
            other => Ok(other.into()),
        }
    }
}

/// The primary type of an album (`MusicBrainz` release group).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ReleaseType)]
pub enum ReleaseTypeDto {
    Album,
    Single,
    Ep,
    Broadcast,
    Other,
}

impl From<ReleaseTypeDto> for ReleaseType {
    fn from(t: ReleaseTypeDto) -> Self {
        match t {
            ReleaseTypeDto::Album => Self::Album,
            ReleaseTypeDto::Single => Self::Single,
            ReleaseTypeDto::Ep => Self::Ep,
            ReleaseTypeDto::Broadcast => Self::Broadcast,
            ReleaseTypeDto::Other => Self::Other,
        }
    }
}

impl From<ReleaseType> for ReleaseTypeDto {
    fn from(t: ReleaseType) -> Self {
        match t {
            ReleaseType::Album => Self::Album,
            ReleaseType::Single => Self::Single,
            ReleaseType::Ep => Self::Ep,
            ReleaseType::Broadcast => Self::Broadcast,
            ReleaseType::Other => Self::Other,
        }
    }
}

/// A secondary type of an album (live, compilation, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = SecondaryType)]
pub enum SecondaryTypeDto {
    Compilation,
    Soundtrack,
    Spokenword,
    Interview,
    Audiobook,
    AudioDrama,
    Live,
    Remix,
    DjMix,
    Mixtape,
    Demo,
    FieldRecording,
}

impl From<SecondaryTypeDto> for SecondaryType {
    fn from(t: SecondaryTypeDto) -> Self {
        match t {
            SecondaryTypeDto::Compilation => Self::Compilation,
            SecondaryTypeDto::Soundtrack => Self::Soundtrack,
            SecondaryTypeDto::Spokenword => Self::Spokenword,
            SecondaryTypeDto::Interview => Self::Interview,
            SecondaryTypeDto::Audiobook => Self::Audiobook,
            SecondaryTypeDto::AudioDrama => Self::AudioDrama,
            SecondaryTypeDto::Live => Self::Live,
            SecondaryTypeDto::Remix => Self::Remix,
            SecondaryTypeDto::DjMix => Self::DjMix,
            SecondaryTypeDto::Mixtape => Self::Mixtape,
            SecondaryTypeDto::Demo => Self::Demo,
            SecondaryTypeDto::FieldRecording => Self::FieldRecording,
        }
    }
}

impl From<SecondaryType> for SecondaryTypeDto {
    fn from(t: SecondaryType) -> Self {
        match t {
            SecondaryType::Compilation => Self::Compilation,
            SecondaryType::Soundtrack => Self::Soundtrack,
            SecondaryType::Spokenword => Self::Spokenword,
            SecondaryType::Interview => Self::Interview,
            SecondaryType::Audiobook => Self::Audiobook,
            SecondaryType::AudioDrama => Self::AudioDrama,
            SecondaryType::Live => Self::Live,
            SecondaryType::Remix => Self::Remix,
            SecondaryType::DjMix => Self::DjMix,
            SecondaryType::Mixtape => Self::Mixtape,
            SecondaryType::Demo => Self::Demo,
            SecondaryType::FieldRecording => Self::FieldRecording,
        }
    }
}

/// Which titles are relevant for a category (D3). Empty lists mean no
/// restriction. Movie and series catalogs (TMDB) use the language,
/// region, provider and genre fields; music catalogs (`MusicBrainz`) only
/// the release types (spec 0006 Q3). A field the catalog does not use is
/// refused with `validation.relevance_not_offered`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = Relevance)]
pub struct RelevanceDto {
    /// ISO 639-1 codes from the catalog's language list.
    #[serde(default)]
    pub original_languages: Vec<String>,
    /// ISO 3166-1 region with watch-provider data.
    #[serde(default)]
    pub provider_region: Option<String>,
    /// Watch provider ids in `providerRegion`.
    #[serde(default)]
    pub providers: Vec<u64>,
    /// ISO 3166-1 region whose release dates count for "Upcoming".
    #[serde(default)]
    pub release_region: Option<String>,
    /// Genre ids to hide.
    #[serde(default)]
    pub excluded_genres: Vec<u64>,
    /// Music: album types shown by default and followed (default album
    /// and EP).
    #[serde(default)]
    pub release_types: Vec<ReleaseTypeDto>,
    /// Music: albums with any of these secondary types are left out
    /// (default: all of them).
    #[serde(default)]
    pub excluded_secondary_types: Vec<SecondaryTypeDto>,
}

impl From<RelevanceDto> for Relevance {
    fn from(r: RelevanceDto) -> Self {
        Self {
            original_languages: r.original_languages,
            provider_region: r.provider_region,
            providers: r.providers,
            release_region: r.release_region,
            excluded_genres: r.excluded_genres,
            release_types: r.release_types.into_iter().map(Into::into).collect(),
            excluded_secondary_types: r
                .excluded_secondary_types
                .into_iter()
                .map(Into::into)
                .collect(),
        }
    }
}

impl From<Relevance> for RelevanceDto {
    fn from(r: Relevance) -> Self {
        Self {
            original_languages: r.original_languages,
            provider_region: r.provider_region,
            providers: r.providers,
            release_region: r.release_region,
            excluded_genres: r.excluded_genres,
            release_types: r.release_types.into_iter().map(Into::into).collect(),
            excluded_secondary_types: r
                .excluded_secondary_types
                .into_iter()
                .map(Into::into)
                .collect(),
        }
    }
}

/// A code with its English name.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NamedCode {
    code: String,
    name: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GenreDto {
    kind: MediaKindDto,
    id: u64,
    name: String,
}

/// The catalog's reference data for editing a relevance.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogReference {
    /// Languages the catalog knows, by English name.
    languages: Vec<NamedCode>,
    /// Countries (release regions), by English name.
    countries: Vec<NamedCode>,
    /// Regions with watch-provider data, by English name.
    provider_regions: Vec<NamedCode>,
    genres: Vec<GenreDto>,
}

/// A watch provider. Provider data comes from `JustWatch` and must be
/// credited where shown.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDto {
    pub(crate) id: u64,
    pub(crate) name: String,
    /// Logo file for `/api/v1/images/tmdb/{size}/{file}` (without slash).
    pub(crate) logo: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ProvidersQuery {
    kind: MediaKindDto,
    /// ISO 3166-1 region.
    region: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewRequest {
    kind: MediaKindDto,
    relevance: RelevanceDto,
}

/// A title in a preview lane.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreviewItem {
    kind: MediaKindDto,
    /// The catalog's id (TMDB id, `MusicBrainz` MBID).
    id: String,
    title: String,
    year: Option<i32>,
    original_language: String,
    /// Image reference for the proxy: a TMDB file for
    /// `/api/v1/images/tmdb/{size}/{ref}` (movies, series) or a release
    /// group MBID for `/api/v1/images/caa/{size}/{ref}` (albums, artists).
    poster: Option<String>,
}

/// The image proxy reference of a catalog image (ADR 0013 convention): TMDB
/// paths come as `/abc.jpg` and become the file name; `caa:<mbid>` becomes
/// the MBID, `stashdb:<uuid>` the `StashDB` image id (scenes, served only
/// through the privacy gate). Which proxy source serves it follows from
/// the title's kind or category.
pub(crate) fn file_name(path: Option<String>) -> Option<String> {
    path.map(|p| {
        match p
            .strip_prefix("caa:")
            .or_else(|| p.strip_prefix("stashdb:"))
        {
            Some(id) => id.to_owned(),
            None => p.trim_start_matches('/').to_owned(),
        }
    })
    .filter(|p| !p.is_empty())
}

impl PreviewItem {
    /// The preview of `e`; `None` for kinds without an API (adult, spec
    /// 0007 slice 2).
    fn of(e: CatalogEntry) -> Option<Self> {
        Some(Self {
            kind: MediaKindDto::of(e.item.kind)?,
            id: e.item.id,
            title: e.title,
            year: e.release_date.map(|d| d.year()),
            original_language: e.original_language,
            poster: file_name(e.poster),
        })
    }
}

fn sorted(mut items: Vec<NamedCode>) -> Vec<NamedCode> {
    items.sort_by_cached_key(|item| item.name.to_lowercase());
    items
}

impl From<CatalogConfiguration> for CatalogReference {
    fn from(c: CatalogConfiguration) -> Self {
        let named = |code: String, name: String| NamedCode {
            name: if name.is_empty() { code.clone() } else { name },
            code,
        };
        Self {
            languages: sorted(
                c.languages
                    .into_iter()
                    .map(|l| named(l.code, l.english_name))
                    .collect(),
            ),
            countries: sorted(
                c.countries
                    .into_iter()
                    .map(|r| named(r.code, r.english_name))
                    .collect(),
            ),
            provider_regions: sorted(
                c.provider_regions
                    .into_iter()
                    .map(|r| named(r.code, r.english_name))
                    .collect(),
            ),
            genres: c
                .genres
                .into_iter()
                .filter_map(|g| {
                    Some(GenreDto {
                        kind: MediaKindDto::of(g.kind)?,
                        id: g.id,
                        name: g.name,
                    })
                })
                .collect(),
        }
    }
}

/// Map a failure to reach the catalog to an API error.
pub(crate) fn adapter_error(err: &AdapterError) -> ApiError {
    tracing::warn!(error = %err, "catalog call failed");
    match err {
        AdapterError::CredentialsInvalid => ApiError::Unavailable("catalog.credentials_invalid"),
        AdapterError::NotFound(_) => ApiError::NotFound("catalog.not_found"),
        _ => ApiError::Unavailable("catalog.unavailable"),
    }
}

/// The catalog adapter of instance `id`, opened with the web key.
pub(crate) async fn open_catalog(state: &AppState, id: i64) -> Result<AnyCatalog, ApiError> {
    state
        .catalog
        .catalog(&state.db, id)
        .await
        .map_err(|err| match err {
            CatalogOpenError::Missing(_) => ApiError::NotFound("services.not_found"),
            CatalogOpenError::NotACatalog { .. } => ApiError::Validation("catalog.not_a_catalog"),
            CatalogOpenError::Unavailable { code, detail } => {
                tracing::warn!(service = id, code, %detail, "catalog unavailable");
                ApiError::Unavailable("catalog.unavailable")
            }
            CatalogOpenError::Db(err) => err.into(),
        })
}

/// Languages, regions and genres of a catalog, for the relevance editor.
#[utoipa::path(get, path = "/api/v1/services/{id}/catalog/reference", tag = "catalog",
    params(("id" = i64, Path, description = "Service id of a catalog")),
    responses(
        (status = 200, body = CatalogReference),
        (status = 404, body = ErrorBody, description = "`services.not_found`"),
        (status = 422, body = ErrorBody, description = "`catalog.not_a_catalog`"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn catalog_reference(
    _admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<CatalogReference>, ApiError> {
    let catalog = open_catalog(&state, id).await?;
    let config = catalog
        .configuration()
        .await
        .map_err(|e| adapter_error(&e))?;
    Ok(Json(config.into()))
}

/// Watch providers of one kind in one region, in the region's order.
#[utoipa::path(get, path = "/api/v1/services/{id}/catalog/providers", tag = "catalog",
    params(("id" = i64, Path, description = "Service id of a catalog"), ProvidersQuery),
    responses(
        (status = 200, body = Vec<ProviderDto>),
        (status = 404, body = ErrorBody, description = "`services.not_found`"),
        (status = 422, body = ErrorBody, description = "`catalog.not_a_catalog`, `validation.relevance_provider_region`, `validation.kind` (`scene`)"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn catalog_providers(
    _admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(query): Query<ProvidersQuery>,
) -> Result<Json<Vec<ProviderDto>>, ApiError> {
    let region = query.region.trim().to_ascii_uppercase();
    if region.len() != 2 || !region.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(ApiError::Validation("validation.relevance_provider_region"));
    }
    let catalog = open_catalog(&state, id).await?;
    let providers = catalog
        .providers(query.kind.tool_kind()?, &region)
        .await
        .map_err(|e| adapter_error(&e))?;
    Ok(Json(
        providers
            .into_iter()
            .map(|p| ProviderDto {
                id: p.id,
                name: p.name,
                logo: file_name(p.logo),
            })
            .collect(),
    ))
}

/// The first page of the Trending lane with the given relevance (D3:
/// preview while editing). Nothing is stored.
#[utoipa::path(post, path = "/api/v1/services/{id}/catalog/preview", tag = "catalog",
    params(("id" = i64, Path, description = "Service id of a catalog")),
    request_body = PreviewRequest,
    responses(
        (status = 200, body = Vec<PreviewItem>),
        (status = 404, body = ErrorBody, description = "`services.not_found`"),
        (status = 422, body = ErrorBody, description = "`catalog.not_a_catalog`, `validation.kind` (`scene`)"),
        (status = 503, body = ErrorBody, description = "`catalog.unavailable`, `catalog.credentials_invalid`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn catalog_preview(
    _admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<PreviewRequest>,
) -> Result<Json<Vec<PreviewItem>>, ApiError> {
    let kind = body.kind.tool_kind()?;
    let catalog = open_catalog(&state, id).await?;
    let relevance: Relevance = body.relevance.into();
    let page = catalog
        .list(&ListQuery {
            kind,
            list: ListKind::Trending,
            page: 1,
            language: PREVIEW_LANGUAGE,
            relevance: &relevance,
            filter: None,
        })
        .await
        .map_err(|e| adapter_error(&e))?;
    Ok(Json(
        page.items.into_iter().filter_map(PreviewItem::of).collect(),
    ))
}

/// A quality profile of a source.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QualityProfileDto {
    id: i64,
    name: String,
}

/// A root folder of a source.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RootFolderDto {
    path: String,
    /// The source can reach it right now.
    accessible: bool,
    free_space_bytes: Option<u64>,
}

/// A metadata profile of a source (Lidarr): which release types an artist
/// keeps after a refresh (spec 0006 Q6).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MetadataProfileDto {
    id: i64,
    name: String,
    /// Allowed primary types as Lidarr names them (`Album`, `EP`, …).
    primary_types: Vec<String>,
    /// Allowed secondary types (`Studio` = none, `Live`, …).
    secondary_types: Vec<String>,
    /// Keeps plain albums and EPs (primary `Album` and `EP`, secondary
    /// `Studio`); otherwise Lidarr may drop a requested album again on its
    /// next refresh, and the admin is warned.
    admits_albums_and_eps: bool,
}

/// Quality profiles, root folders and metadata profiles as last read from
/// a source.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceListsDto {
    quality_profiles: Vec<QualityProfileDto>,
    root_folders: Vec<RootFolderDto>,
    /// Lidarr only; empty for Radarr and Sonarr.
    metadata_profiles: Vec<MetadataProfileDto>,
    /// Last successful read; absent if none succeeded yet.
    fetched_at: Option<DateTime<Utc>>,
    /// Error code of the last read, if it failed (taxonomy as for checks).
    error: Option<String>,
    /// Last read attempt; absent if none ran yet.
    attempted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListsQueued {
    job_id: i64,
}

async fn require_source(state: &AppState, id: i64) -> Result<String, ApiError> {
    let service = state
        .db
        .get_service(id)
        .await?
        .ok_or(ApiError::NotFound("services.not_found"))?;
    let is_source = starr_worker::adapters::descriptor(&service.adapter_type).is_some_and(|d| {
        d.capabilities
            .contains(&starr_core::adapters::Capability::Requests)
    });
    if !is_source {
        return Err(ApiError::Validation("sources.not_a_source"));
    }
    Ok(service.name)
}

/// Quality profiles, root folders and metadata profiles of a source, as
/// last read.
#[utoipa::path(get, path = "/api/v1/services/{id}/source-lists", tag = "catalog",
    params(("id" = i64, Path, description = "Service id of a source")),
    responses(
        (status = 200, body = SourceListsDto),
        (status = 404, body = ErrorBody, description = "`services.not_found`"),
        (status = 422, body = ErrorBody, description = "`sources.not_a_source`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn get_source_lists(
    _admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<SourceListsDto>, ApiError> {
    require_source(&state, id).await?;
    let lists = state.db.source_lists(id).await?;
    Ok(Json(match lists {
        Some(l) => SourceListsDto {
            quality_profiles: l
                .quality_profiles
                .into_iter()
                .map(|p| QualityProfileDto {
                    id: p.id,
                    name: p.name,
                })
                .collect(),
            root_folders: l
                .root_folders
                .into_iter()
                .map(|f| RootFolderDto {
                    path: f.path,
                    accessible: f.accessible,
                    free_space_bytes: f.free_space_bytes,
                })
                .collect(),
            metadata_profiles: l
                .metadata_profiles
                .into_iter()
                .map(|p| MetadataProfileDto {
                    admits_albums_and_eps: p.admits_albums_and_eps(),
                    id: p.id,
                    name: p.name,
                    primary_types: p.primary_types,
                    secondary_types: p.secondary_types,
                })
                .collect(),
            fetched_at: l.fetched_at,
            error: l.error,
            attempted_at: Some(l.attempted_at),
        },
        None => SourceListsDto {
            quality_profiles: Vec::new(),
            root_folders: Vec::new(),
            metadata_profiles: Vec::new(),
            fetched_at: None,
            error: None,
            attempted_at: None,
        },
    }))
}

/// Read a source's quality profiles, root folders and metadata profiles
/// again (in the worker); follow the job, then fetch the lists.
#[utoipa::path(post, path = "/api/v1/services/{id}/source-lists/refresh", tag = "catalog",
    params(("id" = i64, Path, description = "Service id of a source")),
    responses(
        (status = 202, body = ListsQueued, description = "Queued; if a read is already pending, that job"),
        (status = 404, body = ErrorBody, description = "`services.not_found`"),
        (status = 422, body = ErrorBody, description = "`sources.not_a_source`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn refresh_source_lists(
    _admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<(StatusCode, Json<ListsQueued>), ApiError> {
    require_source(&state, id).await?;
    let job_id = enqueue_source_lists(&state, id).await?;
    Ok((StatusCode::ACCEPTED, Json(ListsQueued { job_id })))
}

/// Queue `sources.lists` for source `id` (a pending one is reused).
pub(crate) async fn enqueue_source_lists(state: &AppState, id: i64) -> Result<i64, ApiError> {
    let now = Utc::now();
    let kind = JobKind::SourcesLists;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_paths_become_file_names() {
        assert_eq!(
            file_name(Some("/abc.jpg".into())).as_deref(),
            Some("abc.jpg")
        );
        assert_eq!(file_name(Some("/".into())), None);
        assert_eq!(
            file_name(Some("caa:b1392450-e666-3926-a536-22c65f834433".into())).as_deref(),
            Some("b1392450-e666-3926-a536-22c65f834433")
        );
        assert_eq!(file_name(None), None);
    }
}
