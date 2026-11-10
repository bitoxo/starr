//! The catalog facts of spec 0008 slice 2 as the API shows them: credits
//! for data that must be attributed, cast and crew, certification and
//! providers of the category's region, episodes, tracklists, and the
//! helpers that pick them per region (Q12 A) or shorten them for cards.

use std::collections::HashMap;

use chrono::NaiveDate;
use serde::Serialize;
use starr_core::catalog::{
    CastMember, Catalog, Certification, CrewMember, CrewRole, Episode, ExternalId, MediaKind,
    NextEpisode, OfferKind, RegionalDate, Relevance, Track, TracklistRelease, WatchOffer,
};
use starr_worker::catalog::AnyCatalog;
use utoipa::ToSchema;

use super::catalog::file_name;

/// Longest overview a card carries, in characters.
pub(crate) const CARD_OVERVIEW_CHARS: usize = 200;
/// Genres a card names at most.
pub(crate) const CARD_GENRES: usize = 3;

/// Data that must be credited where it is shown: watch providers
/// (`JustWatch`, TMDB terms) and `MusicBrainz` genres (CC BY-NC-SA,
/// spec 0008 Q17 B).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Attribution {
    /// What is credited: `watch_providers` or `genres`.
    pub(crate) data: AttributedData,
    /// Who provides it (`JustWatch`, `MusicBrainz`).
    pub(crate) source: String,
    /// The licence of the data, where it has one.
    pub(crate) licence: Option<String>,
    /// Where the credit links to: the licence, else the source.
    pub(crate) url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttributedData {
    WatchProviders,
    Genres,
}

impl Attribution {
    pub(crate) fn justwatch() -> Self {
        Self {
            data: AttributedData::WatchProviders,
            source: "JustWatch".to_owned(),
            licence: None,
            url: "https://www.justwatch.com".to_owned(),
        }
    }

    pub(crate) fn musicbrainz_genres() -> Self {
        Self {
            data: AttributedData::Genres,
            source: "MusicBrainz".to_owned(),
            licence: Some("CC BY-NC-SA 3.0".to_owned()),
            url: "https://creativecommons.org/licenses/by-nc-sa/3.0/".to_owned(),
        }
    }
}

/// A cast member (not clickable, spec 0008 Q15 A).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CastDto {
    name: String,
    character: String,
    /// Portrait file for `/api/v1/images/tmdb/{size}/{file}`.
    profile: Option<String>,
}

impl From<CastMember> for CastDto {
    fn from(c: CastMember) -> Self {
        Self {
            name: c.name,
            character: c.character,
            profile: file_name(c.profile),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = CrewRole)]
pub enum CrewRoleDto {
    Director,
    Creator,
    Writer,
}

/// A crew member by role: directors (movies), creators (series), writers.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CrewDto {
    name: String,
    role: CrewRoleDto,
}

impl From<CrewMember> for CrewDto {
    fn from(c: CrewMember) -> Self {
        Self {
            name: c.name,
            role: match c.role {
                CrewRole::Director => CrewRoleDto::Director,
                CrewRole::Creator => CrewRoleDto::Creator,
                CrewRole::Writer => CrewRoleDto::Writer,
            },
        }
    }
}

/// An age rating in the category's region.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CertificationDto {
    /// ISO 3166-1.
    region: String,
    rating: String,
}

/// The next episode of a running series.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NextEpisodeDto {
    season: u32,
    episode: u32,
    name: String,
    air_date: Option<NaiveDate>,
}

impl From<NextEpisode> for NextEpisodeDto {
    fn from(e: NextEpisode) -> Self {
        Self {
            season: e.season,
            episode: e.episode,
            name: e.name,
            air_date: e.air_date,
        }
    }
}

/// How a provider offers a title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = OfferKind)]
pub enum OfferKindDto {
    /// In a subscription.
    Flatrate,
    Free,
    Ads,
    Rent,
    Buy,
}

/// Where to watch a title in the category's provider region (data from
/// `JustWatch`, credited through the details' attributions).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WatchProviderDto {
    region: String,
    kind: OfferKindDto,
    id: u64,
    name: String,
    /// Logo file for `/api/v1/images/tmdb/{size}/{file}`.
    logo: Option<String>,
}

impl From<WatchOffer> for WatchProviderDto {
    fn from(o: WatchOffer) -> Self {
        Self {
            region: o.region,
            kind: match o.kind {
                OfferKind::Flatrate => OfferKindDto::Flatrate,
                OfferKind::Free => OfferKindDto::Free,
                OfferKind::Ads => OfferKindDto::Ads,
                OfferKind::Rent => OfferKindDto::Rent,
                OfferKind::Buy => OfferKindDto::Buy,
            },
            id: o.provider_id,
            name: o.provider_name,
            logo: file_name(o.logo),
        }
    }
}

/// The title's ids in other systems.
#[derive(Debug, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExternalIdsDto {
    tmdb: Option<u64>,
    imdb: Option<String>,
    tvdb: Option<u64>,
}

impl ExternalIdsDto {
    pub(crate) fn of(ids: &[ExternalId]) -> Self {
        let mut out = Self::default();
        for id in ids {
            match id {
                ExternalId::Tmdb(v) => out.tmdb = Some(*v),
                ExternalId::Tvdb(v) => out.tvdb = Some(*v),
                ExternalId::Imdb(v) => out.imdb = Some(v.clone()),
                _ => {}
            }
        }
        out
    }
}

/// The detail view's "Details" tab.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TitleExtras {
    /// ISO 639-1 as the catalog lists it.
    pub(crate) original_language: String,
    /// Production companies.
    pub(crate) companies: Vec<String>,
    pub(crate) external_ids: ExternalIdsDto,
}

/// An episode of a season.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeDto {
    number: u32,
    name: String,
    air_date: Option<NaiveDate>,
    runtime_minutes: Option<u32>,
}

impl From<Episode> for EpisodeDto {
    fn from(e: Episode) -> Self {
        Self {
            number: e.number,
            name: e.name,
            air_date: e.air_date,
            runtime_minutes: e.runtime_minutes,
        }
    }
}

/// A track of an album's tracklist.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrackDto {
    /// 1-based disc number.
    disc: u32,
    /// The disc's own title; empty without one.
    disc_title: String,
    position: u32,
    title: String,
    length_seconds: Option<u32>,
    /// Only where it differs from the album's artist credit.
    artist: Option<String>,
}

impl From<Track> for TrackDto {
    fn from(t: Track) -> Self {
        Self {
            disc: t.disc,
            disc_title: t.disc_title,
            position: t.position,
            title: t.title,
            length_seconds: t.length_seconds,
            artist: t.artist_credit,
        }
    }
}

/// The release an album's tracklist (and track count) is of.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TracklistReleaseDto {
    date: Option<NaiveDate>,
    /// ISO 3166-1 (`XW` worldwide).
    country: Option<String>,
    /// Releases of the album; more than one means "Tracklist of the
    /// <year> release".
    release_count: u32,
}

impl From<TracklistRelease> for TracklistReleaseDto {
    fn from(r: TracklistRelease) -> Self {
        Self {
            date: r.date,
            country: r.country,
            release_count: r.release_count,
        }
    }
}

/// The region of providers (Q12 A: `providerRegion`).
pub(crate) fn provider_region(relevance: &Relevance) -> Option<&str> {
    relevance.provider_region.as_deref()
}

/// The region of certifications and release dates (Q12 A:
/// `releaseRegion`, else `providerRegion`).
pub(crate) fn release_region(relevance: &Relevance) -> Option<&str> {
    relevance
        .release_region
        .as_deref()
        .or(relevance.provider_region.as_deref())
}

/// The certification of `region`.
pub(crate) fn certification(
    certifications: &[Certification],
    region: Option<&str>,
) -> Option<CertificationDto> {
    let region = region?;
    certifications
        .iter()
        .find(|c| c.country.eq_ignore_ascii_case(region))
        .map(|c| CertificationDto {
            region: c.country.clone(),
            rating: c.rating.clone(),
        })
}

/// The digital release in `region`, else the earliest anywhere.
pub(crate) fn digital_release(dates: &[RegionalDate], region: Option<&str>) -> Option<NaiveDate> {
    region
        .and_then(|r| dates.iter().find(|d| d.country.eq_ignore_ascii_case(r)))
        .map(|d| d.date)
        .or_else(|| dates.iter().map(|d| d.date).min())
}

/// An overview for a card: at most [`CARD_OVERVIEW_CHARS`] characters,
/// cut at a word with an ellipsis; `None` when empty.
pub(crate) fn short_overview(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= CARD_OVERVIEW_CHARS {
        return Some(text.to_owned());
    }
    let cut: String = text.chars().take(CARD_OVERVIEW_CHARS - 1).collect();
    let cut = match cut.rfind(char::is_whitespace) {
        Some(at) if at > CARD_OVERVIEW_CHARS / 2 => &cut[..at],
        _ => cut.as_str(),
    };
    Some(format!("{}…", cut.trim_end()))
}

/// Genre names by kind and catalog id, for cards.
pub(crate) type GenreNames = HashMap<(MediaKind, u64), String>;

/// The genre names of a catalog from its (cached) configuration; empty
/// when the catalog has none or cannot be read (cards then name none).
pub(crate) async fn genre_names(catalog: &AnyCatalog) -> GenreNames {
    match catalog.configuration().await {
        Ok(config) => config
            .genres
            .into_iter()
            .map(|g| ((g.kind, g.id), g.name))
            .collect(),
        Err(err) => {
            tracing::warn!(error = %err, "catalog genres unavailable");
            GenreNames::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_overviews_are_cut_at_a_word() {
        assert_eq!(short_overview("  "), None);
        assert_eq!(short_overview(" Short. ").as_deref(), Some("Short."));
        let long = "word ".repeat(100);
        let short = short_overview(&long).unwrap();
        assert!(short.chars().count() <= CARD_OVERVIEW_CHARS, "{short}");
        assert!(short.ends_with("word…"), "{short}");
        let one = "x".repeat(500);
        assert_eq!(
            short_overview(&one).unwrap().chars().count(),
            CARD_OVERVIEW_CHARS
        );
    }

    #[test]
    fn regions_follow_the_relevance() {
        let mut r = Relevance {
            provider_region: Some("CH".into()),
            ..Relevance::default()
        };
        assert_eq!(release_region(&r), Some("CH"));
        r.release_region = Some("DE".into());
        assert_eq!(release_region(&r), Some("DE"));
        assert_eq!(provider_region(&r), Some("CH"));
        let certs = [
            Certification {
                country: "CH".into(),
                rating: "12".into(),
            },
            Certification {
                country: "DE".into(),
                rating: "16".into(),
            },
        ];
        let c = certification(&certs, release_region(&r)).unwrap();
        assert_eq!((c.region.as_str(), c.rating.as_str()), ("DE", "16"));
        assert!(certification(&certs, None).is_none());
        let date = |d| NaiveDate::from_ymd_opt(2026, 1, d).unwrap();
        let dates = [
            RegionalDate {
                country: "US".into(),
                date: date(5),
            },
            RegionalDate {
                country: "DE".into(),
                date: date(9),
            },
        ];
        assert_eq!(digital_release(&dates, Some("DE")), Some(date(9)));
        assert_eq!(digital_release(&dates, Some("FR")), Some(date(5)));
        assert_eq!(digital_release(&[], Some("DE")), None);
    }
}
