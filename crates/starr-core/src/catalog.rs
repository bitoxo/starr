//! The `Catalog` capability (ADR 0009, spec 0003 D3–D5, D12–D13): browsing,
//! searching and describing titles of a metadata service. Pure types and
//! the trait; TMDB implements it in `crates/adapters/tmdb`.
//!
//! Kept small and service-neutral so a music or adult catalog can implement
//! it later: titles are addressed by [`CatalogRef`] (kind plus the catalog's
//! own id as text), lists by [`ListKind`] with a page cursor, and relevance
//! is a value passed per call instead of adapter state.

use std::future::Future;

use chrono::NaiveDate;

use crate::adapters::{AdapterError, Probe};

/// What kind of title an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MediaKind {
    Movie,
    Series,
    /// A music artist (followed, never requested; spec 0006 Q4).
    Artist,
    /// A music album: a `MusicBrainz` release group, the unit of a music
    /// request.
    Album,
    /// An adult scene (`StashDB`), the unit of an adult request (spec 0007
    /// Q3).
    Scene,
    /// An adult performer (followed, never requested; spec 0007 Q3).
    Performer,
    /// An adult studio (followed, never requested; spec 0007 Q3).
    Studio,
}

impl MediaKind {
    pub const ALL: [Self; 7] = [
        Self::Movie,
        Self::Series,
        Self::Artist,
        Self::Album,
        Self::Scene,
        Self::Performer,
        Self::Studio,
    ];

    /// Stable name used in the database and the API.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Series => "series",
            Self::Artist => "artist",
            Self::Album => "album",
            Self::Scene => "scene",
            Self::Performer => "performer",
            Self::Studio => "studio",
        }
    }
}

/// A title in one catalog: its kind and the catalog's own id (TMDB: the
/// numeric id as text).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogRef {
    pub kind: MediaKind,
    pub id: String,
}

/// Separator of a season ref's id: `<series id>/season/<number>`.
const SEASON_SEPARATOR: &str = "/season/";

impl CatalogRef {
    /// The ref of one season of `series` (spec 0008 slice 2): its
    /// [`Catalog::details`] answers the season with its episodes as the
    /// only child, without a trait method of its own. Those details
    /// describe the season, not the series: never record them as the
    /// series' catalog item.
    #[must_use]
    pub fn season(series: &str, number: u32) -> Self {
        Self {
            kind: MediaKind::Series,
            id: format!("{series}{SEASON_SEPARATOR}{number}"),
        }
    }

    /// The series id and season number of a season ref; `None` for any
    /// other ref.
    #[must_use]
    pub fn season_parts(&self) -> Option<(&str, u32)> {
        if self.kind != MediaKind::Series {
            return None;
        }
        let (series, number) = self.id.split_once(SEASON_SEPARATOR)?;
        let number = number.parse().ok()?;
        (!series.is_empty()).then_some((series, number))
    }
}

/// An identifier of the same title in another system (natural keys,
/// lesson 7; ADR 0012).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExternalId {
    /// TMDB id; movie or TV id by the title's kind.
    Tmdb(u64),
    /// TVDB series id.
    Tvdb(u64),
    Imdb(String),
    /// `MusicBrainz` artist MBID.
    MbArtist(uuid::Uuid),
    /// `MusicBrainz` release-group MBID (an album).
    MbReleaseGroup(uuid::Uuid),
    StashScene(uuid::Uuid),
    StashPerformer(uuid::Uuid),
    StashStudio(uuid::Uuid),
}

/// A title as it appears in lists and search results. Text fields are in
/// the requested language with the English fallback already applied (D5).
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry {
    pub item: CatalogRef,
    pub title: String,
    pub original_title: String,
    /// ISO 639-1 code as the catalog reports it (TMDB also uses `xx`, `cn`).
    pub original_language: String,
    pub overview: String,
    /// Movies: primary release; series: first air date.
    pub release_date: Option<NaiveDate>,
    /// Image path for the image proxy (TMDB: `/abc….jpg`).
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub genre_ids: Vec<u64>,
    /// Average rating 0–10 and the number of votes behind it.
    pub rating: Option<f32>,
    pub vote_count: u64,
    /// Albums only: what the music catalog says beyond the common fields.
    pub album: Option<AlbumFacts>,
    /// Scenes only: what the adult catalog says beyond the common fields
    /// (spec 0007 story 6).
    pub scene: Option<SceneFacts>,
}

/// A named title another entry points at: a scene's studio or performers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedRef {
    pub item: CatalogRef,
    pub name: String,
}

/// What an adult catalog knows about a scene beyond [`CatalogEntry`]: its
/// studio, performers and running time, plus its description on the scene
/// page. Identifying and display fields only (spec 0007 "stashdb", amended
/// by spec 0008 Q14: the description; no tags or fingerprints).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SceneFacts {
    /// Running time in seconds, if the catalog knows it.
    pub duration_seconds: Option<u32>,
    pub studio: Option<NamedRef>,
    pub performers: Vec<NamedRef>,
    /// The scene's description as plain text, from the scene's details
    /// only (never lists or search). Free text that may name performers
    /// and describe content: treated like a title by the privacy model
    /// (gate only; never logged, audited, notified or stored outside the
    /// adult catalog cache).
    pub details: Option<String>,
}

/// The primary type of a `MusicBrainz` release group (spec 0006 Q3).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseType {
    Album,
    Single,
    Ep,
    Broadcast,
    Other,
}

impl ReleaseType {
    pub const ALL: [Self; 5] = [
        Self::Album,
        Self::Single,
        Self::Ep,
        Self::Broadcast,
        Self::Other,
    ];

    /// Stable name used in stored settings and the API.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Album => "album",
            Self::Single => "single",
            Self::Ep => "ep",
            Self::Broadcast => "broadcast",
            Self::Other => "other",
        }
    }
}

/// A secondary type of a `MusicBrainz` release group (spec 0006 Q3).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SecondaryType {
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

impl SecondaryType {
    pub const ALL: [Self; 12] = [
        Self::Compilation,
        Self::Soundtrack,
        Self::Spokenword,
        Self::Interview,
        Self::Audiobook,
        Self::AudioDrama,
        Self::Live,
        Self::Remix,
        Self::DjMix,
        Self::Mixtape,
        Self::Demo,
        Self::FieldRecording,
    ];

    /// Stable name used in stored settings and the API.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compilation => "compilation",
            Self::Soundtrack => "soundtrack",
            Self::Spokenword => "spokenword",
            Self::Interview => "interview",
            Self::Audiobook => "audiobook",
            Self::AudioDrama => "audio_drama",
            Self::Live => "live",
            Self::Remix => "remix",
            Self::DjMix => "dj_mix",
            Self::Mixtape => "mixtape",
            Self::Demo => "demo",
            Self::FieldRecording => "field_recording",
        }
    }
}

/// What a music catalog knows about an album beyond [`CatalogEntry`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlbumFacts {
    /// The artist credit as the catalog prints it (e.g. `A & B`).
    pub artist_credit: String,
    /// The first credited artist, for the artist page.
    pub artist: Option<uuid::Uuid>,
    pub primary_type: Option<ReleaseType>,
    pub secondary_types: Vec<SecondaryType>,
    /// Tracks of the release the album is counted by; details only.
    pub track_count: Option<u32>,
    /// The tracklist of that release, in disc and track order; details
    /// only (spec 0008 slice 2).
    pub tracks: Vec<Track>,
    /// Which release the tracklist and track count are of; details only.
    pub tracklist_release: Option<TracklistRelease>,
    /// The first label of that release; details only.
    pub label: Option<String>,
    /// Genre names, most voted first; details only. `MusicBrainz` genres
    /// are supplementary data under CC BY-NC-SA (spec 0008 Q17 B).
    pub genres: Vec<String>,
}

/// One track of an album's tracklist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    /// 1-based disc (medium) number.
    pub disc: u32,
    /// The disc's own title; empty without one.
    pub disc_title: String,
    /// 1-based position on the disc.
    pub position: u32,
    pub title: String,
    pub length_seconds: Option<u32>,
    /// The track's artist credit, only where it differs from the album's.
    pub artist_credit: Option<String>,
}

/// The release an album's tracklist is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TracklistRelease {
    pub date: Option<NaiveDate>,
    /// ISO 3166-1 code (or `MusicBrainz`'s `XW` for worldwide).
    pub country: Option<String>,
    /// Releases of the album in all; more than one means the tracklist is
    /// that of one of them.
    pub release_count: u32,
}

/// A season of a series. Specials are season 0 and flagged (TMDB lists them
/// like any other season).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Season {
    pub number: u32,
    pub name: String,
    pub episode_count: u32,
    pub air_date: Option<NaiveDate>,
    pub is_special: bool,
    pub poster: Option<String>,
    /// The season's episodes: only in the details of a season ref
    /// ([`CatalogRef::season`]), empty in the series' details.
    pub episodes: Vec<Episode>,
}

/// An episode of a season.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Episode {
    pub number: u32,
    pub name: String,
    pub air_date: Option<NaiveDate>,
    pub runtime_minutes: Option<u32>,
}

/// A trailer on a video site (D13: the first `YouTube` trailer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trailer {
    /// e.g. `YouTube`.
    pub site: String,
    /// The site's video key (`YouTube`: the `v=` value).
    pub key: String,
    pub name: String,
}

/// An age rating in one country.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Certification {
    /// ISO 3166-1 code.
    pub country: String,
    pub rating: String,
}

/// How a provider offers a title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OfferKind {
    /// Included in a subscription.
    Flatrate,
    Free,
    Ads,
    Rent,
    Buy,
}

/// A title is offered by a watch provider in a region. Provider data comes
/// from `JustWatch` and must credit it where shown (TMDB terms).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WatchOffer {
    pub region: String,
    pub provider_id: u64,
    pub kind: OfferKind,
    /// The provider's name and logo as the title's offers name them.
    pub provider_name: String,
    pub logo: Option<String>,
}

/// A person in the cast (spec 0008 Q15 A: shown, not browsed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastMember {
    pub name: String,
    pub character: String,
    /// Portrait image path for the image proxy.
    pub profile: Option<String>,
}

/// What a crew member did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrewRole {
    Director,
    /// Created the series.
    Creator,
    Writer,
}

/// A person in the crew, by role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrewMember {
    pub name: String,
    pub role: CrewRole,
}

/// A date in one country (ISO 3166-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionalDate {
    pub country: String,
    pub date: NaiveDate,
}

/// The next episode of a running series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextEpisode {
    pub season: u32,
    pub episode: u32,
    pub name: String,
    pub air_date: Option<NaiveDate>,
}

/// What a catalog says about a series as a whole.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeriesFacts {
    /// The catalog's status text (TMDB: `Returning Series`, `Ended`,
    /// `Canceled`, `In Production`, `Planned`, `Pilot`).
    pub status: String,
    pub season_count: Option<u32>,
    pub next_episode: Option<NextEpisode>,
    pub networks: Vec<String>,
}

/// Facts of a movie or series for the detail view (spec 0008 slice 2);
/// empty for catalogs that have none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TitleFacts {
    pub runtime_minutes: Option<u32>,
    pub tagline: String,
    /// Top billed cast, at most [`CAST_LIMIT`].
    pub cast: Vec<CastMember>,
    /// Directors (movies), creators (series) and writers.
    pub crew: Vec<CrewMember>,
    pub series: Option<SeriesFacts>,
    /// Movies: the earliest digital release per country.
    pub digital_releases: Vec<RegionalDate>,
    pub production_companies: Vec<String>,
}

/// How many cast members the detail view shows (spec 0003 D13 as amended
/// by spec 0008 Q15 A).
pub const CAST_LIMIT: usize = 12;

/// What a title is made of: the seasons of a series or the albums of an
/// artist (spec 0006 Q12, ADR 0013).
#[derive(Debug, Clone, PartialEq)]
pub enum Child {
    Season(Season),
    /// An album in an artist's discography; its `item` is the album's ref.
    Album(Box<CatalogEntry>),
    /// A scene of a performer or studio page (spec 0007 Q15); its `item` is
    /// the scene's ref.
    Scene(Box<CatalogEntry>),
}

/// Everything the detail view needs (D13), plus the IDs the source
/// adapters key on (D14).
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogDetails {
    pub entry: CatalogEntry,
    pub external_ids: Vec<ExternalId>,
    pub genres: Vec<Genre>,
    pub trailer: Option<Trailer>,
    pub certifications: Vec<Certification>,
    pub watch_offers: Vec<WatchOffer>,
    /// Seasons of a series, albums of an artist; empty for movies and
    /// albums.
    pub children: Vec<Child>,
    /// Runtime, cast, crew and series facts (TMDB); empty elsewhere.
    pub facts: TitleFacts,
}

impl CatalogDetails {
    /// The seasons among the children (series only).
    pub fn seasons(&self) -> impl Iterator<Item = &Season> {
        self.children.iter().filter_map(|child| match child {
            Child::Season(season) => Some(season),
            Child::Album(_) | Child::Scene(_) => None,
        })
    }

    /// The albums among the children (an artist's discography).
    pub fn albums(&self) -> impl Iterator<Item = &CatalogEntry> {
        self.children.iter().filter_map(|child| match child {
            Child::Album(album) => Some(&**album),
            Child::Season(_) | Child::Scene(_) => None,
        })
    }

    /// The scenes among the children (a performer's or studio's scenes).
    pub fn scenes(&self) -> impl Iterator<Item = &CatalogEntry> {
        self.children.iter().filter_map(|child| match child {
            Child::Scene(scene) => Some(&**scene),
            Child::Season(_) | Child::Album(_) => None,
        })
    }
}

/// The browse lists of spec 0003 D4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListKind {
    /// The catalog's own weekly trend, filtered by original language only.
    Trending,
    Popular,
    /// First releases in the release region, from today on.
    Upcoming,
    /// Recent titles on the configured providers in the provider region.
    OnServices,
    /// Highest rated with a minimum number of votes.
    TopRated,
    /// Recent and announced releases (music, spec 0006 Q2).
    FreshReleases,
    /// Newest releases first (adult scenes by date, spec 0007 Q15; movies
    /// and series released up to today: the "release date" sort of a
    /// category page's grid, spec 0008 slice 2).
    New,
    /// Series with an episode airing in the coming days (spec 0008).
    OnTheAir,
}

impl ListKind {
    pub const ALL: [Self; 8] = [
        Self::Trending,
        Self::Popular,
        Self::Upcoming,
        Self::OnServices,
        Self::TopRated,
        Self::FreshReleases,
        Self::New,
        Self::OnTheAir,
    ];

    /// Stable name used in the database and the API.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trending => "trending",
            Self::Popular => "popular",
            Self::Upcoming => "upcoming",
            Self::OnServices => "on_services",
            Self::TopRated => "top_rated",
            Self::FreshReleases => "fresh_releases",
            Self::New => "new",
            Self::OnTheAir => "on_the_air",
        }
    }
}

/// Which titles are relevant for a category (D3). Applied to browse lists,
/// never to search. Empty lists mean "no restriction".
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Relevance {
    /// Original languages (ISO 639-1 as the catalog lists them); any of them.
    pub original_languages: Vec<String>,
    /// Region (ISO 3166-1) for watch providers.
    pub provider_region: Option<String>,
    /// Watch provider ids in `provider_region`; any of them.
    pub providers: Vec<u64>,
    /// Region (ISO 3166-1) whose release dates count for "Upcoming".
    pub release_region: Option<String>,
    /// Genres to hide (series default: Talk, News, Soap, Reality).
    pub excluded_genres: Vec<u64>,
    /// Music: primary release types shown by default and followed (spec
    /// 0006 Q3); empty means every type.
    pub release_types: Vec<ReleaseType>,
    /// Music: albums with any of these secondary types are left out.
    pub excluded_secondary_types: Vec<SecondaryType>,
}

impl Relevance {
    /// Where a music catalog's relevance starts (spec 0006 Q3 B): albums
    /// and EPs, no secondary types.
    #[must_use]
    pub fn music_default() -> Self {
        Self {
            release_types: vec![ReleaseType::Album, ReleaseType::Ep],
            excluded_secondary_types: SecondaryType::ALL.to_vec(),
            ..Self::default()
        }
    }

    /// Whether an album of these types is relevant. Music only: movie and
    /// series relevances have no release types and admit every album.
    #[must_use]
    pub fn admits_album(&self, primary: Option<ReleaseType>, secondary: &[SecondaryType]) -> bool {
        let type_ok = self.release_types.is_empty()
            || primary.is_some_and(|p| self.release_types.contains(&p));
        type_ok
            && !secondary
                .iter()
                .any(|s| self.excluded_secondary_types.contains(s))
    }
}

/// A genre as the catalog keys it: TMDB by numeric id, `MusicBrainz` by
/// name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GenreRef {
    Id(u64),
    Name(String),
}

/// Narrows a browse list (spec 0008 Q20 A).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ListFilter {
    /// Titles in the subscription of this watch provider in the relevance's
    /// provider region ("On \<provider\>": TMDB cannot prove exclusivity).
    Provider(u64),
    Genre(GenreRef),
}

/// A browse request: one list of one kind, one page.
#[derive(Debug, Clone)]
pub struct ListQuery<'a> {
    pub kind: MediaKind,
    pub list: ListKind,
    /// 1-based page cursor from [`Page::next`] (start with 1).
    pub page: u32,
    /// BCP 47 tag, e.g. `en-US`.
    pub language: &'a str,
    pub relevance: &'a Relevance,
    /// Narrows the list; a catalog that cannot apply it to this list
    /// answers an empty page.
    pub filter: Option<ListFilter>,
}

/// A search request across the given kinds (D12: no people).
#[derive(Debug, Clone, Copy)]
pub struct SearchQuery<'a> {
    pub text: &'a str,
    pub kinds: &'a [MediaKind],
    pub page: u32,
    pub language: &'a str,
}

/// One page of a list. `next` is `None` at the end of the list, which
/// includes the catalog's page cap: running into it is not an error.
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<u32>,
}

impl<T> Page<T> {
    /// A page with nothing on it and nothing after it.
    #[must_use]
    pub fn end() -> Self {
        Self {
            items: Vec::new(),
            next: None,
        }
    }
}

/// A genre of one media kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Genre {
    pub kind: MediaKind,
    /// The catalog's numeric id; 0 where the catalog keys genres by name
    /// (`MusicBrainz`).
    pub id: u64,
    pub name: String,
}

/// A language the catalog knows (validate against this list, not ISO).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Language {
    pub code: String,
    pub english_name: String,
    /// Name in the language itself; may be empty.
    pub native_name: String,
}

/// A country or region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// ISO 3166-1 code.
    pub code: String,
    pub english_name: String,
    pub native_name: String,
}

/// Image sizes the catalog's image host serves, per image type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageSizes {
    pub poster: Vec<String>,
    pub backdrop: Vec<String>,
    pub logo: Vec<String>,
    pub profile: Vec<String>,
    pub still: Vec<String>,
}

impl ImageSizes {
    /// Whether any image type is served in `size`.
    #[must_use]
    pub fn contains(&self, size: &str) -> bool {
        [
            &self.poster,
            &self.backdrop,
            &self.logo,
            &self.profile,
            &self.still,
        ]
        .into_iter()
        .any(|sizes| sizes.iter().any(|s| s == size))
    }
}

/// Reference data for configuring relevance and rendering images.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogConfiguration {
    pub image_sizes: ImageSizes,
    pub languages: Vec<Language>,
    pub countries: Vec<Region>,
    /// Regions with watch-provider data.
    pub provider_regions: Vec<Region>,
    pub genres: Vec<Genre>,
}

/// A watch provider in one region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchProvider {
    pub id: u64,
    pub name: String,
    pub logo: Option<String>,
}

/// A metadata catalog (capability `metadata`).
pub trait Catalog: Probe {
    /// Reference data: image sizes, languages, countries, provider regions,
    /// genres.
    fn configuration(
        &self,
    ) -> impl Future<Output = Result<CatalogConfiguration, AdapterError>> + Send;

    /// Watch providers of `kind` in `region`, in the region's display order.
    fn providers(
        &self,
        kind: MediaKind,
        region: &str,
    ) -> impl Future<Output = Result<Vec<WatchProvider>, AdapterError>> + Send;

    /// Search titles of the requested kinds; relevance does not apply (D3).
    fn search(
        &self,
        query: &SearchQuery<'_>,
    ) -> impl Future<Output = Result<Page<CatalogEntry>, AdapterError>> + Send;

    /// One browse list page, filtered by the query's relevance.
    fn list(
        &self,
        query: &ListQuery<'_>,
    ) -> impl Future<Output = Result<Page<CatalogEntry>, AdapterError>> + Send;

    /// Details of one title. Unknown ids are [`AdapterError::NotFound`].
    fn details(
        &self,
        item: &CatalogRef,
        language: &str,
    ) -> impl Future<Output = Result<CatalogDetails, AdapterError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_sizes_match_any_type() {
        let sizes = ImageSizes {
            poster: vec!["w92".into(), "original".into()],
            still: vec!["w300".into()],
            ..ImageSizes::default()
        };
        assert!(sizes.contains("w92"));
        assert!(sizes.contains("w300"));
        assert!(!sizes.contains("w93"));
        assert!(!sizes.contains(""));
    }

    #[test]
    fn names_are_stable_and_unique() {
        let lists: Vec<_> = ListKind::ALL.iter().map(|l| l.as_str()).collect();
        let mut unique = lists.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(lists.len(), unique.len());
        assert_eq!(MediaKind::Series.as_str(), "series");
        assert_eq!(ListKind::FreshReleases.as_str(), "fresh_releases");
        let kinds: Vec<_> = MediaKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "movie",
                "series",
                "artist",
                "album",
                "scene",
                "performer",
                "studio"
            ]
        );
        assert_eq!(ListKind::New.as_str(), "new");
        assert_eq!(ListKind::OnTheAir.as_str(), "on_the_air");
    }

    #[test]
    fn season_refs_round_trip_and_nothing_else_is_one() {
        let season = CatalogRef::season("1399", 2);
        assert_eq!(season.kind, MediaKind::Series);
        assert_eq!(season.season_parts(), Some(("1399", 2)));
        for id in ["1399", "/season/2", "1399/season/x", "1399/season/"] {
            let r = CatalogRef {
                kind: MediaKind::Series,
                id: id.into(),
            };
            assert_eq!(r.season_parts(), None, "{id}");
        }
        let movie = CatalogRef {
            kind: MediaKind::Movie,
            id: "1399/season/2".into(),
        };
        assert_eq!(movie.season_parts(), None);
    }

    fn entry(kind: MediaKind, id: &str) -> CatalogEntry {
        CatalogEntry {
            item: CatalogRef {
                kind,
                id: id.into(),
            },
            title: String::new(),
            original_title: String::new(),
            original_language: String::new(),
            overview: String::new(),
            release_date: None,
            poster: None,
            backdrop: None,
            genre_ids: Vec::new(),
            rating: None,
            vote_count: 0,
            album: None,
            scene: None,
        }
    }

    #[test]
    fn music_default_admits_albums_and_eps_without_secondary_types() {
        let r = Relevance::music_default();
        assert!(r.admits_album(Some(ReleaseType::Album), &[]));
        assert!(r.admits_album(Some(ReleaseType::Ep), &[]));
        assert!(!r.admits_album(Some(ReleaseType::Single), &[]));
        assert!(!r.admits_album(None, &[]));
        assert!(!r.admits_album(Some(ReleaseType::Album), &[SecondaryType::Live]));
        // No restriction at all: everything.
        assert!(Relevance::default().admits_album(None, &[SecondaryType::Live]));
    }

    #[test]
    fn music_names_are_stable_and_match_serde() {
        for t in ReleaseType::ALL {
            let json = serde_json::to_string(&t).unwrap();
            assert_eq!(json, format!("\"{}\"", t.as_str()));
        }
        for t in SecondaryType::ALL {
            let json = serde_json::to_string(&t).unwrap();
            assert_eq!(json, format!("\"{}\"", t.as_str()));
        }
    }

    #[test]
    fn relevance_reads_movie_and_music_json() {
        let tmdb: Relevance = serde_json::from_str(r#"{"originalLanguages":["en"]}"#).unwrap();
        assert_eq!(tmdb.release_types, []);
        let music: Relevance = serde_json::from_str(
            r#"{"releaseTypes":["album","ep"],"excludedSecondaryTypes":["live"]}"#,
        )
        .unwrap();
        assert_eq!(music.release_types, [ReleaseType::Album, ReleaseType::Ep]);
        assert_eq!(music.excluded_secondary_types, [SecondaryType::Live]);
    }

    #[test]
    fn seasons_are_the_season_children() {
        let season = |number| Season {
            number,
            name: format!("Season {number}"),
            episode_count: 8,
            air_date: None,
            is_special: number == 0,
            poster: None,
            episodes: Vec::new(),
        };
        let details = CatalogDetails {
            entry: entry(MediaKind::Series, "1396"),
            external_ids: Vec::new(),
            genres: Vec::new(),
            trailer: None,
            certifications: Vec::new(),
            watch_offers: Vec::new(),
            children: vec![
                Child::Season(season(0)),
                Child::Album(Box::new(entry(MediaKind::Album, "x"))),
                Child::Season(season(1)),
            ],
            facts: TitleFacts::default(),
        };
        let numbers: Vec<_> = details.seasons().map(|s| s.number).collect();
        assert_eq!(numbers, [0, 1]);
    }
}
