//! Categories and service roles (domain model §2–3, spec 0003 D3, D9):
//! which service instance does what for a category, and the per-role
//! settings. Pure rules; storage lives in `starr-db`.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::adapters::Capability;
use crate::catalog::{CatalogConfiguration, MediaKind, Relevance, WatchProvider};
use crate::members::Category;
use crate::source::{MetadataProfile, QualityProfile, RootFolder};

/// What a service instance does for a category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RoleKind {
    /// Discover, search, metadata; no member account.
    Catalog,
    /// Requests go here; no member account. At most one per category.
    Source,
    /// Members watch or listen here ("open in").
    Consume,
}

impl RoleKind {
    pub const ALL: [Self; 3] = [Self::Catalog, Self::Source, Self::Consume];

    /// Stable name used in the database and the API.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Source => "source",
            Self::Consume => "consume",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// The capability an instance needs to take this role (domain model
    /// §3: roles only by capability). Consume needs `libraries`: phase 1
    /// reads availability and builds links (spec 0003, O3); member accounts
    /// in the service come with phase 2.
    #[must_use]
    pub const fn required_capability(self) -> Capability {
        match self {
            Self::Catalog => Capability::Metadata,
            Self::Source => Capability::Requests,
            Self::Consume => Capability::Libraries,
        }
    }

    /// Whether an instance offering `offered` may take this role.
    #[must_use]
    pub fn allowed_for(self, offered: &[Capability]) -> bool {
        offered.contains(&self.required_capability())
    }
}

/// The TMDB media kind a category's catalog and source deal in; `None`
/// for categories TMDB does not serve (music, adult).
#[must_use]
pub const fn media_kind(category: Category) -> Option<MediaKind> {
    match category {
        Category::Movies => Some(MediaKind::Movie),
        Category::Series => Some(MediaKind::Series),
        Category::Music | Category::Adult => None,
    }
}

/// The kinds a category's catalog serves: what Discover lists and search
/// finds there. Music: artists and albums (spec 0006 Q11); adult: scenes,
/// performers and studios (`StashDB`, spec 0007 slice 2).
#[must_use]
pub const fn catalog_kinds(category: Category) -> &'static [MediaKind] {
    match category {
        Category::Movies => &[MediaKind::Movie],
        Category::Series => &[MediaKind::Series],
        Category::Music => &[MediaKind::Artist, MediaKind::Album],
        Category::Adult => &[MediaKind::Scene, MediaKind::Performer, MediaKind::Studio],
    }
}

/// Whether a catalog adapter exists for `category`, so it can have catalog
/// roles and Discover (music since spec 0006 slice 2, adult since spec
/// 0007 slice 2).
#[must_use]
pub const fn has_catalog(category: Category) -> bool {
    !catalog_kinds(category).is_empty()
}

/// Whether catalog and source adapters exist for `category`, so members
/// can request there (ADR 0012). Every category can be switched on and
/// watched in (consume roles) regardless; music has its request flow
/// since spec 0006 slice 4 (`MusicBrainz` + Lidarr), adult since spec
/// 0007 slice 4 (scenes: `StashDB` + Whisparr, behind the privacy gate).
#[must_use]
pub const fn has_request_flow(category: Category) -> bool {
    has_catalog(category) && has_source(category)
}

/// Whether a source adapter exists for `category`, so the admin can set
/// up its source (Lidarr for music since spec 0006 slice 3, Whisparr for
/// adult since spec 0007 slice 3) before members can request there
/// ([`has_request_flow`]).
#[must_use]
pub const fn has_source(category: Category) -> bool {
    match category {
        Category::Movies | Category::Series | Category::Music | Category::Adult => true,
    }
}

/// The privacy gate of the adult category (spec 0007, "The gate"): adult
/// content is visible to a viewer only when the category is switched on
/// (which implies the operator statement, Q12), the viewer may see it (a
/// member with the adult entitlement switched on, or an admin, Q9) and the
/// viewer's session has unlocked it (Q7). API handlers, the image route
/// and the in-app notification lists decide with this; "not visible" means
/// absent, never refused with a reason.
#[must_use]
pub const fn adult_visible(
    category_enabled: bool,
    entitlement: bool,
    session_unlocked: bool,
) -> bool {
    category_enabled && entitlement && session_unlocked
}

/// Whether `category` is private: its titles, names, images and reasons
/// never leave the gate (external channels, audit, logs, run reports).
#[must_use]
pub const fn is_private(category: Category) -> bool {
    matches!(category, Category::Adult)
}

/// Whether content of `category` is visible to a viewer for whom
/// [`adult_visible`] answered `adult`. Other categories are not gated here
/// (entitlements decide them as before).
#[must_use]
pub const fn category_visible(category: Category, adult: bool) -> bool {
    !is_private(category) || adult
}

/// Whether `category` may have a role of `kind`: catalogs need a catalog
/// adapter, sources a source adapter, consume roles nothing.
#[must_use]
pub const fn role_allowed_in(kind: RoleKind, category: Category) -> bool {
    match kind {
        RoleKind::Catalog => has_catalog(category),
        RoleKind::Source => has_source(category),
        RoleKind::Consume => true,
    }
}

/// When Radarr considers a movie available for download (its
/// `minimumAvailability`). Movies only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MinimumAvailability {
    Announced,
    InCinemas,
    #[default]
    Released,
}

impl MinimumAvailability {
    /// The value Radarr's API uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Announced => "announced",
            Self::InCinemas => "inCinemas",
            Self::Released => "released",
        }
    }
}

/// Settings of a category's source role (D9): one quality profile and root
/// folder per category, picked from the source's own lists.
///
/// The JSON form is what `service_role.settings` stores. It keeps exactly
/// the shape releases before spec 0006 slice 3 read (they refuse unknown
/// fields and would fail the whole category list), so
/// [`Self::metadata_profile_id`] is not part of it: storage keeps it in a
/// column of its own (ADR 0008 rollback).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSettings {
    pub quality_profile_id: Option<i64>,
    pub root_folder_path: Option<String>,
    /// Movies only; `None` for series.
    pub minimum_availability: Option<MinimumAvailability>,
    /// Music only (Lidarr, spec 0006 Q6); `None` for movies and series.
    #[serde(skip)]
    pub metadata_profile_id: Option<i64>,
}

impl SourceSettings {
    /// Clear what the source's lists no longer offer (spec 0008 ADM-01):
    /// after its address changed, the lists read from there decide whether
    /// it is still the same server. Returns whether anything was cleared.
    pub fn retain_listed(
        &mut self,
        profiles: &[QualityProfile],
        folders: &[RootFolder],
        metadata_profiles: &[MetadataProfile],
    ) -> bool {
        let mut cleared = false;
        if self
            .quality_profile_id
            .is_some_and(|id| !profiles.iter().any(|p| p.id == id))
        {
            self.quality_profile_id = None;
            cleared = true;
        }
        if self
            .root_folder_path
            .as_ref()
            .is_some_and(|path| !folders.iter().any(|f| &f.path == path))
        {
            self.root_folder_path = None;
            cleared = true;
        }
        if self
            .metadata_profile_id
            .is_some_and(|id| !metadata_profiles.iter().any(|p| p.id == id))
        {
            self.metadata_profile_id = None;
            cleared = true;
        }
        cleared
    }
}

/// Upper bounds on list sizes, so a relevance stays a small value.
pub const RELEVANCE_MAX_LANGUAGES: usize = 50;
pub const RELEVANCE_MAX_PROVIDERS: usize = 100;
pub const RELEVANCE_MAX_GENRES: usize = 50;

/// Why a relevance was refused; [`RelevanceError::code`] names the field.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RelevanceError {
    #[error("unknown original language {0:?}")]
    Language(String),
    #[error("unknown release region {0:?}")]
    ReleaseRegion(String),
    #[error("unknown provider region {0:?}")]
    ProviderRegion(String),
    #[error("providers need a provider region")]
    ProvidersWithoutRegion,
    #[error("unknown provider {0} in the provider region")]
    Provider(u64),
    #[error("unknown genre {0}")]
    Genre(u64),
    #[error("too many entries")]
    TooMany,
    /// A field the category's catalog does not use (e.g. release types
    /// for TMDB, languages for `MusicBrainz`).
    #[error("this catalog offers no such setting")]
    NotOffered,
}

impl RelevanceError {
    /// Validation code for the API (ADR 0007).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Language(_) => "validation.relevance_language",
            Self::ReleaseRegion(_) => "validation.relevance_release_region",
            Self::ProviderRegion(_) => "validation.relevance_provider_region",
            Self::ProvidersWithoutRegion | Self::Provider(_) => "validation.relevance_provider",
            Self::Genre(_) => "validation.relevance_genre",
            Self::TooMany => "validation.relevance_too_many",
            Self::NotOffered => "validation.relevance_not_offered",
        }
    }
}

fn dedup<T: Clone + Eq + std::hash::Hash>(items: Vec<T>) -> Vec<T> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.clone()))
        .collect()
}

/// Check `input` against the catalog's own reference data (languages from
/// the catalog's list, not an ISO table; providers of `kind` in the
/// provider region) and return it normalised: codes trimmed, duplicates
/// removed, empty regions dropped. `providers` must be the catalog's
/// providers of `kind` in `input.provider_region` (empty without one).
pub fn validate_relevance(
    kind: MediaKind,
    input: Relevance,
    config: &CatalogConfiguration,
    providers: &[WatchProvider],
) -> Result<Relevance, RelevanceError> {
    let region = |raw: Option<String>| {
        raw.map(|r| r.trim().to_ascii_uppercase())
            .filter(|r| !r.is_empty())
    };
    let original_languages = dedup(
        input
            .original_languages
            .into_iter()
            .map(|l| l.trim().to_ascii_lowercase())
            .collect(),
    );
    let provider_region = region(input.provider_region);
    let release_region = region(input.release_region);
    let providers_wanted = dedup(input.providers);
    let excluded_genres = dedup(input.excluded_genres);
    if original_languages.len() > RELEVANCE_MAX_LANGUAGES
        || providers_wanted.len() > RELEVANCE_MAX_PROVIDERS
        || excluded_genres.len() > RELEVANCE_MAX_GENRES
    {
        return Err(RelevanceError::TooMany);
    }
    if !input.release_types.is_empty() || !input.excluded_secondary_types.is_empty() {
        return Err(RelevanceError::NotOffered);
    }

    if let Some(unknown) = original_languages
        .iter()
        .find(|code| !config.languages.iter().any(|l| &l.code == *code))
    {
        return Err(RelevanceError::Language(unknown.clone()));
    }
    if let Some(r) = &release_region
        && !config.countries.iter().any(|c| &c.code == r)
    {
        return Err(RelevanceError::ReleaseRegion(r.clone()));
    }
    if let Some(r) = &provider_region
        && !config.provider_regions.iter().any(|c| &c.code == r)
    {
        return Err(RelevanceError::ProviderRegion(r.clone()));
    }
    if provider_region.is_none() && !providers_wanted.is_empty() {
        return Err(RelevanceError::ProvidersWithoutRegion);
    }
    if let Some(unknown) = providers_wanted
        .iter()
        .find(|id| !providers.iter().any(|p| p.id == **id))
    {
        return Err(RelevanceError::Provider(*unknown));
    }
    if let Some(unknown) = excluded_genres
        .iter()
        .find(|id| !config.genres.iter().any(|g| g.kind == kind && g.id == **id))
    {
        return Err(RelevanceError::Genre(*unknown));
    }
    Ok(Relevance {
        original_languages,
        provider_region,
        providers: providers_wanted,
        release_region,
        excluded_genres,
        release_types: Vec::new(),
        excluded_secondary_types: Vec::new(),
    })
}

/// Check a music catalog's relevance (spec 0006 Q3): only release types
/// and excluded secondary types; the movie and series fields are not
/// offered. Duplicates are removed, the order is the types' own.
pub fn validate_music_relevance(input: Relevance) -> Result<Relevance, RelevanceError> {
    let blank = |r: &Option<String>| r.as_deref().is_none_or(|r| r.trim().is_empty());
    if !input.original_languages.is_empty()
        || !blank(&input.provider_region)
        || !input.providers.is_empty()
        || !blank(&input.release_region)
        || !input.excluded_genres.is_empty()
    {
        return Err(RelevanceError::NotOffered);
    }
    let mut release_types = input.release_types;
    release_types.sort_unstable();
    release_types.dedup();
    let mut excluded_secondary_types = input.excluded_secondary_types;
    excluded_secondary_types.sort_unstable();
    excluded_secondary_types.dedup();
    Ok(Relevance {
        release_types,
        excluded_secondary_types,
        ..Relevance::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Genre, Language, Region};

    #[test]
    fn source_settings_keep_what_the_lists_still_offer() {
        let profiles = [QualityProfile {
            id: 4,
            name: "HD".into(),
        }];
        let folders = [RootFolder {
            id: 1,
            path: "/music/".into(),
            accessible: true,
            free_space_bytes: None,
        }];
        let metadata = [MetadataProfile {
            id: 2,
            name: "Standard".into(),
            primary_types: vec![],
            secondary_types: vec![],
        }];
        let chosen = SourceSettings {
            quality_profile_id: Some(4),
            root_folder_path: Some("/music/".into()),
            minimum_availability: None,
            metadata_profile_id: Some(2),
        };
        // The same server: nothing changes.
        let mut same = chosen.clone();
        assert!(!same.retain_listed(&profiles, &folders, &metadata));
        assert_eq!(same, chosen);
        // Another server: only what it lacks goes.
        let mut other = chosen.clone();
        assert!(other.retain_listed(&profiles, &[], &[]));
        assert_eq!(
            other,
            SourceSettings {
                root_folder_path: None,
                metadata_profile_id: None,
                ..chosen
            }
        );
        // Nothing chosen: nothing to clear.
        assert!(!SourceSettings::default().retain_listed(&[], &[], &[]));
    }

    #[test]
    fn adult_is_visible_only_when_every_gate_step_holds() {
        for enabled in [false, true] {
            for entitled in [false, true] {
                for unlocked in [false, true] {
                    assert_eq!(
                        adult_visible(enabled, entitled, unlocked),
                        enabled && entitled && unlocked,
                        "{enabled} {entitled} {unlocked}"
                    );
                }
            }
        }
    }

    #[test]
    fn only_adult_is_private_and_gated() {
        for category in Category::ALL {
            assert_eq!(is_private(category), category == Category::Adult);
            assert!(category_visible(category, true));
            assert_eq!(
                category_visible(category, false),
                category != Category::Adult
            );
        }
    }

    fn config() -> CatalogConfiguration {
        let region = |code: &str| Region {
            code: code.into(),
            english_name: code.into(),
            native_name: String::new(),
        };
        CatalogConfiguration {
            languages: ["en", "de", "ko"]
                .map(|code| Language {
                    code: code.into(),
                    english_name: code.into(),
                    native_name: String::new(),
                })
                .to_vec(),
            countries: vec![region("CH"), region("DE"), region("US")],
            provider_regions: vec![region("CH"), region("DE")],
            genres: vec![
                Genre {
                    kind: MediaKind::Series,
                    id: 10767,
                    name: "Talk".into(),
                },
                Genre {
                    kind: MediaKind::Movie,
                    id: 27,
                    name: "Horror".into(),
                },
            ],
            ..CatalogConfiguration::default()
        }
    }

    fn netflix() -> Vec<WatchProvider> {
        vec![WatchProvider {
            id: 8,
            name: "Netflix".into(),
            logo: None,
        }]
    }

    #[test]
    fn roles_need_their_capability() {
        assert!(RoleKind::Catalog.allowed_for(&[Capability::Metadata]));
        assert!(!RoleKind::Catalog.allowed_for(&[Capability::Requests]));
        assert!(RoleKind::Source.allowed_for(&[Capability::Requests]));
        assert!(!RoleKind::Source.allowed_for(&[Capability::Metadata]));
        assert!(RoleKind::Consume.allowed_for(&[Capability::Libraries]));
        assert!(!RoleKind::Consume.allowed_for(&[Capability::Accounts]));
        for kind in RoleKind::ALL {
            assert_eq!(RoleKind::parse(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn only_movies_and_series_have_a_media_kind_in_phase_1() {
        assert_eq!(media_kind(Category::Movies), Some(MediaKind::Movie));
        assert_eq!(media_kind(Category::Series), Some(MediaKind::Series));
        assert_eq!(media_kind(Category::Music), None);
        assert_eq!(media_kind(Category::Adult), None);
    }

    #[test]
    fn every_category_has_its_request_flow_since_spec_0007_slice_4() {
        assert!(has_request_flow(Category::Movies));
        assert!(has_request_flow(Category::Series));
        assert!(has_request_flow(Category::Music));
        // Adult: `StashDB` (slice 2) and Whisparr (slice 3), scenes only.
        assert!(has_request_flow(Category::Adult));
        assert!(has_catalog(Category::Music));
        assert!(has_catalog(Category::Adult));
        assert_eq!(
            catalog_kinds(Category::Adult),
            [MediaKind::Scene, MediaKind::Performer, MediaKind::Studio]
        );
        assert_eq!(
            catalog_kinds(Category::Music),
            [MediaKind::Artist, MediaKind::Album]
        );
        assert!(role_allowed_in(RoleKind::Catalog, Category::Music));
        // Lidarr can be set up before music requests open (slice 3).
        assert!(role_allowed_in(RoleKind::Source, Category::Music));
        // Whisparr can be set up before adult requests open (spec 0007
        // slices 3 and 4).
        assert!(role_allowed_in(RoleKind::Source, Category::Adult));
        assert!(has_source(Category::Adult));
        assert!(role_allowed_in(RoleKind::Consume, Category::Adult));
        assert!(role_allowed_in(RoleKind::Catalog, Category::Adult));
        assert!(role_allowed_in(RoleKind::Source, Category::Movies));
    }

    #[test]
    fn music_relevance_takes_only_release_types() {
        use crate::catalog::{ReleaseType, SecondaryType};
        let out = validate_music_relevance(Relevance {
            release_types: vec![ReleaseType::Ep, ReleaseType::Album, ReleaseType::Ep],
            excluded_secondary_types: vec![SecondaryType::Live, SecondaryType::Live],
            release_region: Some(" ".into()),
            ..Relevance::default()
        })
        .unwrap();
        assert_eq!(out.release_types, [ReleaseType::Album, ReleaseType::Ep]);
        assert_eq!(out.excluded_secondary_types, [SecondaryType::Live]);
        assert_eq!(out.release_region, None);
        let err = validate_music_relevance(Relevance {
            original_languages: vec!["en".into()],
            ..Relevance::default()
        })
        .unwrap_err();
        assert_eq!(err.code(), "validation.relevance_not_offered");
        // And the other way round: TMDB takes no release types.
        let err = validate_relevance(
            MediaKind::Movie,
            Relevance {
                release_types: vec![ReleaseType::Album],
                ..Relevance::default()
            },
            &config(),
            &[],
        )
        .unwrap_err();
        assert_eq!(err, RelevanceError::NotOffered);
    }

    #[test]
    fn valid_relevance_is_normalised() {
        let input = Relevance {
            original_languages: vec![" EN".into(), "de".into(), "en".into()],
            provider_region: Some("ch".into()),
            providers: vec![8, 8],
            release_region: Some(" ".into()),
            excluded_genres: vec![10767],
            ..Relevance::default()
        };
        let out = validate_relevance(MediaKind::Series, input, &config(), &netflix()).unwrap();
        assert_eq!(out.original_languages, vec!["en", "de"]);
        assert_eq!(out.provider_region.as_deref(), Some("CH"));
        assert_eq!(out.providers, vec![8]);
        assert_eq!(out.release_region, None);
        assert_eq!(out.excluded_genres, vec![10767]);
        assert_eq!(
            validate_relevance(MediaKind::Movie, Relevance::default(), &config(), &[]),
            Ok(Relevance::default())
        );
    }

    #[test]
    fn unknown_reference_values_are_refused() {
        let check = |r: Relevance, kind| validate_relevance(kind, r, &config(), &netflix());
        let series = MediaKind::Series;
        let err = |r| check(r, series).unwrap_err();
        // ISO 639-1 has `fr`, but the catalog's list decides.
        assert_eq!(
            err(Relevance {
                original_languages: vec!["fr".into()],
                ..Relevance::default()
            }),
            RelevanceError::Language("fr".into())
        );
        assert_eq!(
            err(Relevance {
                release_region: Some("XX".into()),
                ..Relevance::default()
            })
            .code(),
            "validation.relevance_release_region"
        );
        // US is a country, but has no provider data in this configuration.
        assert_eq!(
            err(Relevance {
                provider_region: Some("US".into()),
                ..Relevance::default()
            }),
            RelevanceError::ProviderRegion("US".into())
        );
        assert_eq!(
            err(Relevance {
                providers: vec![8],
                ..Relevance::default()
            }),
            RelevanceError::ProvidersWithoutRegion
        );
        assert_eq!(
            err(Relevance {
                provider_region: Some("DE".into()),
                providers: vec![9],
                ..Relevance::default()
            }),
            RelevanceError::Provider(9)
        );
        // Horror is a movie genre, not a series genre.
        assert_eq!(
            err(Relevance {
                excluded_genres: vec![27],
                ..Relevance::default()
            }),
            RelevanceError::Genre(27)
        );
        assert_eq!(
            err(Relevance {
                excluded_genres: (0..60).collect(),
                ..Relevance::default()
            }),
            RelevanceError::TooMany
        );
    }

    #[test]
    fn minimum_availability_defaults_to_released_and_uses_radarr_names() {
        assert_eq!(
            MinimumAvailability::default(),
            MinimumAvailability::Released
        );
        assert_eq!(MinimumAvailability::InCinemas.as_str(), "inCinemas");
        let json = serde_json::to_string(&MinimumAvailability::InCinemas).unwrap();
        assert_eq!(json, "\"inCinemas\"");
    }

    /// What releases before spec 0006 slice 3 read from
    /// `service_role.settings` (0.3.0's `SourceSettings`).
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    #[allow(dead_code)]
    struct SourceSettingsV0_3 {
        quality_profile_id: Option<i64>,
        root_folder_path: Option<String>,
        minimum_availability: Option<MinimumAvailability>,
    }

    #[test]
    fn stored_source_settings_stay_readable_for_the_previous_release() {
        let music = SourceSettings {
            quality_profile_id: Some(1),
            root_folder_path: Some("/music".into()),
            minimum_availability: None,
            metadata_profile_id: Some(3),
        };
        let json = serde_json::to_string(&music).unwrap();
        assert!(!json.contains("metadata"), "{json}");
        serde_json::from_str::<SourceSettingsV0_3>(&json).unwrap();
        // And a stored value from that release still reads today.
        let old =
            r#"{"qualityProfileId":4,"rootFolderPath":"/movies","minimumAvailability":"released"}"#;
        let read: SourceSettings = serde_json::from_str(old).unwrap();
        assert_eq!(read.metadata_profile_id, None);
        assert_eq!(read.quality_profile_id, Some(4));
    }
}
