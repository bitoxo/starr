//! TMDB v3 response shapes and their mapping to the core catalog types.
//! Only the fields Starr reads are declared; required ones are not
//! optional, so a changed shape fails loudly as `Unknown` (lesson 10).

use std::collections::{BTreeMap, HashSet};

use chrono::NaiveDate;
use serde::Deserialize;
use starr_core::catalog::{
    CAST_LIMIT, CastMember, CatalogEntry, CatalogRef, Certification, CrewMember, CrewRole, Episode,
    ExternalId, Genre, Language, MediaKind, NextEpisode, OfferKind, Region, RegionalDate, Season,
    SeriesFacts, TitleFacts, Trailer, WatchOffer, WatchProvider,
};

/// Writers the detail view lists at most.
const WRITER_LIMIT: usize = 6;

/// TMDB's release type of a theatrical release, and of a digital one.
const RELEASE_THEATRICAL: u8 = 3;
const RELEASE_DIGITAL: u8 = 4;

/// A paged list (`search`, `trending`, `discover`).
#[derive(Debug, Deserialize)]
pub struct PageWire {
    pub results: Vec<ItemWire>,
    pub total_pages: u32,
}

/// A movie or series in a list. Movies use `title`/`release_date`, series
/// `name`/`first_air_date`; `media_type` is present in mixed lists only.
#[derive(Debug, Deserialize)]
pub struct ItemWire {
    pub id: u64,
    pub media_type: Option<String>,
    pub title: Option<String>,
    pub name: Option<String>,
    pub original_title: Option<String>,
    pub original_name: Option<String>,
    #[serde(default)]
    pub original_language: Option<String>,
    #[serde(default)]
    pub overview: Option<String>,
    pub release_date: Option<String>,
    pub first_air_date: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    #[serde(default)]
    pub genre_ids: Vec<u64>,
    pub vote_average: Option<f32>,
    #[serde(default)]
    pub vote_count: u64,
}

/// `YYYY-MM-DD`, also the date part of `…T00:00:00.000Z`; empty → `None`.
pub fn parse_date(raw: Option<&str>) -> Option<NaiveDate> {
    let raw = raw?.trim();
    NaiveDate::parse_from_str(raw.get(..10)?, "%Y-%m-%d").ok()
}

fn non_empty(path: Option<String>) -> Option<String> {
    path.filter(|p| !p.trim().is_empty())
}

/// An image path the proxy can serve (`/` + a valid file name); anything
/// else from upstream is dropped here, at the adapter boundary.
fn image_path(path: Option<String>) -> Option<String> {
    path.filter(|p| p.strip_prefix('/').is_some_and(crate::images::valid_file))
}

/// `YouTube` video ids are 11 characters of `[A-Za-z0-9_-]`; other keys are
/// not linked.
fn youtube_key(key: &str) -> bool {
    key.len() == 11
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl ItemWire {
    /// The kind of a mixed-list item; `None` for people and anything else.
    pub fn mixed_kind(&self) -> Option<MediaKind> {
        match self.media_type.as_deref() {
            Some("movie") => Some(MediaKind::Movie),
            Some("tv") => Some(MediaKind::Series),
            _ => None,
        }
    }

    /// `kind` is one TMDB serves (`params::KINDS`); TV fields for series,
    /// movie fields otherwise.
    pub fn into_entry(self, kind: MediaKind) -> CatalogEntry {
        let (title, original_title, date) = if kind == MediaKind::Series {
            (self.name, self.original_name, self.first_air_date)
        } else {
            (self.title, self.original_title, self.release_date)
        };
        let title = title.unwrap_or_default();
        CatalogEntry {
            item: CatalogRef {
                kind,
                id: self.id.to_string(),
            },
            original_title: original_title.unwrap_or_else(|| title.clone()),
            title,
            original_language: self.original_language.unwrap_or_default(),
            overview: self.overview.unwrap_or_default(),
            release_date: parse_date(date.as_deref()),
            poster: image_path(self.poster_path),
            backdrop: image_path(self.backdrop_path),
            genre_ids: self.genre_ids,
            rating: self.vote_average.filter(|_| self.vote_count > 0),
            vote_count: self.vote_count,
            album: None,
            scene: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct GenreWire {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct GenreListWire {
    pub genres: Vec<GenreWire>,
}

impl GenreListWire {
    pub fn into_genres(self, kind: MediaKind) -> impl Iterator<Item = Genre> {
        self.genres.into_iter().map(move |g| Genre {
            kind,
            id: g.id,
            name: g.name,
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct Results<T> {
    pub results: T,
}

#[derive(Debug, Default, Deserialize)]
pub struct ExternalIdsWire {
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct VideoWire {
    pub site: String,
    pub key: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub official: bool,
}

#[derive(Debug, Deserialize)]
pub struct CountryReleasesWire {
    pub iso_3166_1: String,
    pub release_dates: Vec<ReleaseDateWire>,
}

#[derive(Debug, Deserialize)]
pub struct ReleaseDateWire {
    #[serde(default)]
    pub certification: String,
    pub release_date: Option<String>,
    /// 1 premiere, 2 theatrical (limited), 3 theatrical, 4 digital, 5
    /// physical, 6 TV.
    #[serde(rename = "type", default)]
    pub kind: Option<u8>,
}

#[derive(Debug, Deserialize)]
pub struct ContentRatingWire {
    pub iso_3166_1: String,
    #[serde(default)]
    pub rating: String,
}

#[derive(Debug, Deserialize)]
pub struct ProviderRefWire {
    pub provider_id: u64,
    #[serde(default)]
    pub provider_name: String,
    #[serde(default)]
    pub logo_path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RegionOffersWire {
    #[serde(default)]
    pub flatrate: Vec<ProviderRefWire>,
    #[serde(default)]
    pub free: Vec<ProviderRefWire>,
    #[serde(default)]
    pub ads: Vec<ProviderRefWire>,
    #[serde(default)]
    pub rent: Vec<ProviderRefWire>,
    #[serde(default)]
    pub buy: Vec<ProviderRefWire>,
}

#[derive(Debug, Deserialize)]
pub struct SeasonWire {
    pub season_number: u32,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub episode_count: Option<u32>,
    pub air_date: Option<String>,
    pub poster_path: Option<String>,
}

/// `GET /3/{movie|tv}/{id}` with `append_to_response`.
#[derive(Debug, Deserialize)]
pub struct DetailsWire {
    #[serde(flatten)]
    pub item: ItemWire,
    #[serde(default)]
    pub genres: Vec<GenreWire>,
    pub external_ids: Option<ExternalIdsWire>,
    pub videos: Option<Results<Vec<VideoWire>>>,
    pub release_dates: Option<Results<Vec<CountryReleasesWire>>>,
    pub content_ratings: Option<Results<Vec<ContentRatingWire>>>,
    #[serde(rename = "watch/providers")]
    pub watch_providers: Option<Results<BTreeMap<String, RegionOffersWire>>>,
    #[serde(default)]
    pub seasons: Vec<SeasonWire>,
    /// Movies: minutes.
    #[serde(default)]
    pub runtime: Option<u32>,
    /// Series: typical episode lengths in minutes (often empty).
    #[serde(default)]
    pub episode_run_time: Vec<u32>,
    #[serde(default)]
    pub tagline: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub number_of_seasons: Option<u32>,
    #[serde(default)]
    pub next_episode_to_air: Option<EpisodeRefWire>,
    #[serde(default)]
    pub networks: Vec<NamedWire>,
    #[serde(default)]
    pub created_by: Vec<NamedWire>,
    #[serde(default)]
    pub production_companies: Vec<NamedWire>,
    /// Movies (`append_to_response=credits`).
    pub credits: Option<CreditsWire>,
    /// Series (`append_to_response=aggregate_credits`).
    pub aggregate_credits: Option<AggregateCreditsWire>,
}

/// Anything TMDB names (network, company, creator).
#[derive(Debug, Deserialize)]
pub struct NamedWire {
    pub name: String,
}

/// `next_episode_to_air` of a series.
#[derive(Debug, Deserialize)]
pub struct EpisodeRefWire {
    pub season_number: u32,
    pub episode_number: u32,
    #[serde(default)]
    pub name: Option<String>,
    pub air_date: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CastWire {
    pub name: String,
    #[serde(default)]
    pub character: Option<String>,
    pub profile_path: Option<String>,
    #[serde(default)]
    pub order: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct CrewWire {
    pub name: String,
    #[serde(default)]
    pub job: String,
    #[serde(default)]
    pub department: String,
}

/// `credits` of a movie.
#[derive(Debug, Default, Deserialize)]
pub struct CreditsWire {
    #[serde(default)]
    pub cast: Vec<CastWire>,
    #[serde(default)]
    pub crew: Vec<CrewWire>,
}

#[derive(Debug, Deserialize)]
pub struct RoleWire {
    #[serde(default)]
    pub character: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AggregateCastWire {
    pub name: String,
    #[serde(default)]
    pub roles: Vec<RoleWire>,
    pub profile_path: Option<String>,
    #[serde(default)]
    pub order: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct AggregateCrewWire {
    pub name: String,
    #[serde(default)]
    pub department: String,
}

/// `aggregate_credits` of a series: roles over all seasons.
#[derive(Debug, Default, Deserialize)]
pub struct AggregateCreditsWire {
    #[serde(default)]
    pub cast: Vec<AggregateCastWire>,
    #[serde(default)]
    pub crew: Vec<AggregateCrewWire>,
}

/// `GET /3/tv/{id}/season/{n}`.
#[derive(Debug, Deserialize)]
pub struct SeasonDetailsWire {
    pub season_number: u32,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub overview: Option<String>,
    pub air_date: Option<String>,
    pub poster_path: Option<String>,
    #[serde(default)]
    pub episodes: Vec<EpisodeWire>,
}

#[derive(Debug, Deserialize)]
pub struct EpisodeWire {
    pub episode_number: u32,
    #[serde(default)]
    pub name: Option<String>,
    pub air_date: Option<String>,
    #[serde(default)]
    pub runtime: Option<u32>,
}

fn names(wire: Vec<NamedWire>) -> Vec<String> {
    wire.into_iter()
        .map(|n| n.name.trim().to_owned())
        .filter(|n| !n.is_empty())
        .collect()
}

/// Top billed cast in billing order, at most [`CAST_LIMIT`].
fn top_cast(mut cast: Vec<(Option<u32>, CastMember)>) -> Vec<CastMember> {
    // Stable: unordered entries keep TMDB's order after the ordered ones.
    cast.sort_by_key(|(order, _)| order.unwrap_or(u32::MAX));
    cast.into_iter().map(|(_, c)| c).take(CAST_LIMIT).collect()
}

/// Crew in the order given, each person once per role, writers capped.
fn crew_of(people: impl Iterator<Item = (String, CrewRole)>) -> Vec<CrewMember> {
    let mut seen = HashSet::new();
    let mut writers = 0;
    let mut out = Vec::new();
    for (name, role) in people {
        if name.trim().is_empty() || !seen.insert((name.clone(), role)) {
            continue;
        }
        if role == CrewRole::Writer {
            if writers == WRITER_LIMIT {
                continue;
            }
            writers += 1;
        }
        out.push(CrewMember { name, role });
    }
    out
}

impl DetailsWire {
    /// The facts of the detail view, taking the appended credits and the
    /// fields not in lists out of the response.
    pub fn take_facts(&mut self, series: bool) -> TitleFacts {
        let positive = |m: Option<u32>| m.filter(|m| *m > 0);
        let runtime_minutes = if series {
            positive(self.episode_run_time.first().copied())
        } else {
            positive(self.runtime)
        };
        let (cast, crew) = if series {
            let credits = self.aggregate_credits.take().unwrap_or_default();
            let cast = credits
                .cast
                .into_iter()
                .map(|c| {
                    let character = c
                        .roles
                        .into_iter()
                        .find_map(|r| r.character.filter(|ch| !ch.trim().is_empty()))
                        .unwrap_or_default();
                    (
                        c.order,
                        CastMember {
                            name: c.name,
                            character,
                            profile: image_path(c.profile_path),
                        },
                    )
                })
                .collect();
            let creators = std::mem::take(&mut self.created_by)
                .into_iter()
                .map(|c| (c.name, CrewRole::Creator));
            let writers = credits
                .crew
                .into_iter()
                .filter(|c| c.department == "Writing")
                .map(|c| (c.name, CrewRole::Writer));
            (cast, crew_of(creators.chain(writers)))
        } else {
            let credits = self.credits.take().unwrap_or_default();
            let cast = credits
                .cast
                .into_iter()
                .map(|c| {
                    (
                        c.order,
                        CastMember {
                            name: c.name,
                            character: c.character.unwrap_or_default(),
                            profile: image_path(c.profile_path),
                        },
                    )
                })
                .collect();
            let (directors, writers): (Vec<CrewWire>, Vec<CrewWire>) = credits
                .crew
                .into_iter()
                .filter(|c| c.job == "Director" || c.department == "Writing")
                .partition(|c| c.job == "Director");
            let people = directors
                .into_iter()
                .map(|c| (c.name, CrewRole::Director))
                .chain(writers.into_iter().map(|c| (c.name, CrewRole::Writer)));
            (cast, crew_of(people))
        };
        let series_facts = series.then(|| SeriesFacts {
            status: self.status.take().unwrap_or_default(),
            season_count: self.number_of_seasons,
            next_episode: self.next_episode_to_air.take().map(|e| NextEpisode {
                season: e.season_number,
                episode: e.episode_number,
                name: e.name.unwrap_or_default(),
                air_date: parse_date(e.air_date.as_deref()),
            }),
            networks: names(std::mem::take(&mut self.networks)),
        });
        TitleFacts {
            runtime_minutes,
            tagline: self.tagline.take().unwrap_or_default().trim().to_owned(),
            cast: top_cast(cast),
            crew,
            series: series_facts,
            digital_releases: Vec::new(),
            production_companies: names(std::mem::take(&mut self.production_companies)),
        }
    }
}

/// The earliest digital release per country (movies).
pub fn digital_releases(countries: &[CountryReleasesWire]) -> Vec<RegionalDate> {
    countries
        .iter()
        .filter_map(|country| {
            let date = country
                .release_dates
                .iter()
                .filter(|r| r.kind == Some(RELEASE_DIGITAL))
                .filter_map(|r| parse_date(r.release_date.as_deref()))
                .min()?;
            Some(RegionalDate {
                country: country.iso_3166_1.clone(),
                date,
            })
        })
        .collect()
}

impl SeasonDetailsWire {
    /// The season with its episodes in episode order.
    pub fn into_season(self) -> (Season, String) {
        let mut episodes: Vec<Episode> = self
            .episodes
            .into_iter()
            .map(|e| Episode {
                number: e.episode_number,
                name: e.name.unwrap_or_default(),
                air_date: parse_date(e.air_date.as_deref()),
                runtime_minutes: e.runtime.filter(|m| *m > 0),
            })
            .collect();
        episodes.sort_by_key(|e| e.number);
        let season = Season {
            number: self.season_number,
            name: self.name.unwrap_or_default(),
            episode_count: u32::try_from(episodes.len()).unwrap_or(u32::MAX),
            air_date: parse_date(self.air_date.as_deref()),
            is_special: self.season_number == 0,
            poster: image_path(self.poster_path),
            episodes,
        };
        (season, self.overview.unwrap_or_default())
    }
}

pub fn external_ids(tmdb_id: u64, wire: Option<ExternalIdsWire>) -> Vec<ExternalId> {
    let wire = wire.unwrap_or_default();
    let mut ids = vec![ExternalId::Tmdb(tmdb_id)];
    if let Some(tvdb) = wire.tvdb_id.filter(|id| *id > 0) {
        ids.push(ExternalId::Tvdb(tvdb));
    }
    if let Some(imdb) = non_empty(wire.imdb_id) {
        ids.push(ExternalId::Imdb(imdb));
    }
    ids
}

/// The first official `YouTube` trailer, else the first `YouTube` trailer.
/// `site` is free text at TMDB; only `YouTube` is linked (D13).
pub fn trailer(videos: Vec<VideoWire>) -> Option<Trailer> {
    let mut trailers: Vec<VideoWire> = videos
        .into_iter()
        .filter(|v| v.site == "YouTube" && v.kind == "Trailer" && youtube_key(&v.key))
        .collect();
    let index = trailers.iter().position(|v| v.official).unwrap_or(0);
    if trailers.is_empty() {
        return None;
    }
    let v = trailers.swap_remove(index);
    Some(Trailer {
        site: v.site,
        key: v.key,
        name: v.name,
    })
}

/// Movie age ratings (spec 0008 slice 2): per country the rating of the
/// theatrical release, else the first non-empty one in TMDB's order. Empty
/// ratings are ignored and values are trimmed (TMDB pads some)
/// (capabilities.md, TMDB "Age ratings").
pub fn movie_certifications(countries: &[CountryReleasesWire]) -> Vec<Certification> {
    countries
        .iter()
        .filter_map(|country| {
            let rated: Vec<(Option<u8>, &str)> = country
                .release_dates
                .iter()
                .map(|r| (r.kind, r.certification.trim()))
                .filter(|(_, rating)| !rating.is_empty())
                .collect();
            let chosen = rated
                .iter()
                .find(|(kind, _)| *kind == Some(RELEASE_THEATRICAL))
                .or_else(|| rated.first())?;
            Some(Certification {
                country: country.iso_3166_1.clone(),
                rating: chosen.1.to_owned(),
            })
        })
        .collect()
}

/// Series age ratings: undated, countries may repeat; the first non-empty
/// rating per country wins.
pub fn series_certifications(ratings: Vec<ContentRatingWire>) -> Vec<Certification> {
    let mut seen = HashSet::new();
    ratings
        .into_iter()
        .filter_map(|r| {
            let rating = r.rating.trim();
            if rating.is_empty() || seen.contains(&r.iso_3166_1) {
                return None;
            }
            seen.insert(r.iso_3166_1.clone());
            Some(Certification {
                country: r.iso_3166_1,
                rating: rating.to_owned(),
            })
        })
        .collect()
}

pub fn watch_offers(regions: BTreeMap<String, RegionOffersWire>) -> Vec<WatchOffer> {
    let mut offers = Vec::new();
    for (region, o) in regions {
        for (kind, list) in [
            (OfferKind::Flatrate, o.flatrate),
            (OfferKind::Free, o.free),
            (OfferKind::Ads, o.ads),
            (OfferKind::Rent, o.rent),
            (OfferKind::Buy, o.buy),
        ] {
            offers.extend(list.into_iter().map(|p| WatchOffer {
                region: region.clone(),
                provider_id: p.provider_id,
                kind,
                provider_name: p.provider_name,
                logo: image_path(p.logo_path),
            }));
        }
    }
    offers
}

pub fn seasons(wire: Vec<SeasonWire>) -> Vec<Season> {
    let mut seasons: Vec<Season> = wire
        .into_iter()
        .map(|s| Season {
            number: s.season_number,
            name: s.name.unwrap_or_default(),
            episode_count: s.episode_count.unwrap_or(0),
            air_date: parse_date(s.air_date.as_deref()),
            is_special: s.season_number == 0,
            poster: image_path(s.poster_path),
            episodes: Vec::new(),
        })
        .collect();
    seasons.sort_by_key(|s| s.number);
    seasons
}

/// `GET /3/configuration`.
#[derive(Debug, Deserialize)]
pub struct ConfigurationWire {
    pub images: ImagesWire,
}

#[derive(Debug, Deserialize)]
#[allow(clippy::struct_field_names)] // TMDB's field names
pub struct ImagesWire {
    pub poster_sizes: Vec<String>,
    pub backdrop_sizes: Vec<String>,
    pub logo_sizes: Vec<String>,
    pub profile_sizes: Vec<String>,
    pub still_sizes: Vec<String>,
}

/// `GET /3/configuration/languages`.
#[derive(Debug, Deserialize)]
pub struct LanguageWire {
    pub iso_639_1: String,
    pub english_name: String,
    #[serde(default)]
    pub name: String,
}

impl From<LanguageWire> for Language {
    fn from(l: LanguageWire) -> Self {
        Self {
            code: l.iso_639_1,
            english_name: l.english_name,
            native_name: l.name,
        }
    }
}

/// `GET /3/configuration/countries` and `/3/watch/providers/regions`.
#[derive(Debug, Deserialize)]
pub struct RegionWire {
    pub iso_3166_1: String,
    pub english_name: String,
    #[serde(default)]
    pub native_name: String,
}

impl From<RegionWire> for Region {
    fn from(r: RegionWire) -> Self {
        Self {
            code: r.iso_3166_1,
            english_name: r.english_name,
            native_name: r.native_name,
        }
    }
}

/// `GET /3/watch/providers/{movie|tv}?watch_region=`.
#[derive(Debug, Deserialize)]
pub struct ProviderWire {
    pub provider_id: u64,
    pub provider_name: String,
    pub logo_path: Option<String>,
    #[serde(default)]
    pub display_priorities: BTreeMap<String, i64>,
}

/// Providers in the region's display order: `display_priorities[region]`,
/// not the global `display_priority` (capabilities.md). Providers without
/// a priority for the region go last, by name.
pub fn providers_in_order(mut wire: Vec<ProviderWire>, region: &str) -> Vec<WatchProvider> {
    wire.sort_by(|a, b| {
        let pa = a
            .display_priorities
            .get(region)
            .copied()
            .unwrap_or(i64::MAX);
        let pb = b
            .display_priorities
            .get(region)
            .copied()
            .unwrap_or(i64::MAX);
        pa.cmp(&pb)
            .then_with(|| a.provider_name.cmp(&b.provider_name))
    });
    wire.into_iter()
        .map(|p| WatchProvider {
            id: p.provider_id,
            name: p.provider_name,
            logo: image_path(p.logo_path),
        })
        .collect()
}

/// `GET /3/authentication`.
#[derive(Debug, Deserialize)]
pub struct AuthenticationWire {
    pub success: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn release(cert: &str, date: &str) -> ReleaseDateWire {
        typed(cert, date, 5)
    }

    fn typed(cert: &str, date: &str, kind: u8) -> ReleaseDateWire {
        ReleaseDateWire {
            certification: cert.to_owned(),
            release_date: Some(format!("{date}T00:00:00.000Z")),
            kind: Some(kind),
        }
    }

    #[test]
    fn dates_parse_with_and_without_time_and_empty_is_none() {
        assert_eq!(parse_date(Some("2010-07-15")), Some(date("2010-07-15")));
        assert_eq!(
            parse_date(Some("2010-07-15T00:00:00.000Z")),
            Some(date("2010-07-15"))
        );
        assert_eq!(parse_date(Some("")), None);
        assert_eq!(parse_date(Some("2010")), None);
        assert_eq!(parse_date(None), None);
    }

    #[test]
    fn movie_rating_is_the_theatrical_else_the_first_non_empty_one() {
        let countries = vec![
            CountryReleasesWire {
                iso_3166_1: "DE".into(),
                release_dates: vec![
                    typed("6", "2010-07-01", 1),
                    typed(" 12 ", "2010-07-29", 3),
                    release("16", "2024-05-01"),
                ],
            },
            // Theatrical without a rating (as Inception in CH): the first
            // non-empty one, the physical release.
            CountryReleasesWire {
                iso_3166_1: "CH".into(),
                release_dates: vec![
                    typed("", "2010-07-29", 3),
                    release(" 12 ", "2010-12-01"),
                    typed("16", "2011-01-01", 4),
                ],
            },
            CountryReleasesWire {
                iso_3166_1: "AT".into(),
                release_dates: vec![release("", "2020-01-01")],
            },
        ];
        assert_eq!(
            movie_certifications(&countries),
            vec![
                Certification {
                    country: "DE".into(),
                    rating: "12".into()
                },
                Certification {
                    country: "CH".into(),
                    rating: "12".into()
                },
            ]
        );
        // Digital releases: the earliest per country.
        let digital = digital_releases(&countries);
        assert_eq!(
            digital,
            vec![RegionalDate {
                country: "CH".into(),
                date: date("2011-01-01")
            }]
        );
    }

    #[test]
    fn movie_facts_take_the_top_cast_directors_and_writers() {
        let raw = serde_json::json!({
            "id": 1, "title": "T", "runtime": 148, "tagline": " A tagline ",
            "production_companies": [{ "name": "Studio One" }, { "name": " " }],
            "credits": {
                "cast": (0..15).rev().map(|i| serde_json::json!({
                    "name": format!("Actor {i}"), "character": format!("Role {i}"),
                    "profile_path": "/abcdefghijklmnopqrstuvwxyz1.jpg", "order": i,
                })).collect::<Vec<_>>(),
                "crew": [
                    { "name": "Writer W", "job": "Screenplay", "department": "Writing" },
                    { "name": "Director D", "job": "Director", "department": "Directing" },
                    { "name": "Writer W", "job": "Story", "department": "Writing" },
                    { "name": "Editor E", "job": "Editor", "department": "Editing" },
                ],
            },
        });
        let mut wire: DetailsWire = serde_json::from_value(raw).unwrap();
        let facts = wire.take_facts(false);
        assert_eq!(facts.runtime_minutes, Some(148));
        assert_eq!(facts.tagline, "A tagline");
        assert_eq!(facts.production_companies, ["Studio One"]);
        assert_eq!(facts.cast.len(), CAST_LIMIT);
        assert_eq!(facts.cast[0].name, "Actor 0");
        assert_eq!(facts.cast[0].character, "Role 0");
        assert!(facts.cast[0].profile.is_some());
        let crew: Vec<_> = facts
            .crew
            .iter()
            .map(|c| (c.name.as_str(), c.role))
            .collect();
        assert_eq!(
            crew,
            [
                ("Director D", CrewRole::Director),
                ("Writer W", CrewRole::Writer)
            ]
        );
        assert_eq!(facts.series, None);
    }

    #[test]
    fn series_facts_have_creators_status_and_the_next_episode() {
        let raw = serde_json::json!({
            "id": 2, "name": "S", "episode_run_time": [], "status": "Returning Series",
            "number_of_seasons": 3,
            "next_episode_to_air": { "season_number": 3, "episode_number": 4, "name": "E4", "air_date": "2026-11-13" },
            "networks": [{ "name": "Network N" }],
            "created_by": [{ "name": "Creator C" }],
            "aggregate_credits": {
                "cast": [
                    { "name": "Actor B", "roles": [{ "character": "" }, { "character": "Role B" }], "order": 1 },
                    { "name": "Actor A", "roles": [{ "character": "Role A" }], "order": 0, "profile_path": "not a path" },
                ],
                "crew": [{ "name": "Writer W", "department": "Writing" }, { "name": "Grip G", "department": "Crew" }],
            },
        });
        let mut wire: DetailsWire = serde_json::from_value(raw).unwrap();
        let facts = wire.take_facts(true);
        assert_eq!(facts.runtime_minutes, None);
        let names: Vec<_> = facts
            .cast
            .iter()
            .map(|c| (c.name.as_str(), c.character.as_str()))
            .collect();
        assert_eq!(names, [("Actor A", "Role A"), ("Actor B", "Role B")]);
        // Not a path the proxy serves.
        assert_eq!(facts.cast[0].profile, None);
        let crew: Vec<_> = facts
            .crew
            .iter()
            .map(|c| (c.name.as_str(), c.role))
            .collect();
        assert_eq!(
            crew,
            [
                ("Creator C", CrewRole::Creator),
                ("Writer W", CrewRole::Writer)
            ]
        );
        let series = facts.series.unwrap();
        assert_eq!(series.status, "Returning Series");
        assert_eq!(series.season_count, Some(3));
        assert_eq!(series.networks, ["Network N"]);
        let next = series.next_episode.unwrap();
        assert_eq!((next.season, next.episode), (3, 4));
        assert_eq!(next.air_date, Some(date("2026-11-13")));
    }

    #[test]
    fn season_details_list_episodes_in_order() {
        let raw = serde_json::json!({
            "season_number": 1, "name": "Season 1", "air_date": "2011-04-17",
            "episodes": [
                { "episode_number": 2, "name": "Two", "air_date": "2011-04-24", "runtime": 56 },
                { "episode_number": 1, "name": "One", "air_date": "2011-04-17", "runtime": 0 },
            ],
        });
        let wire: SeasonDetailsWire = serde_json::from_value(raw).unwrap();
        let (season, _) = wire.into_season();
        assert_eq!(season.episode_count, 2);
        assert_eq!(season.episodes[0].number, 1);
        assert_eq!(season.episodes[0].runtime_minutes, None);
        assert_eq!(season.episodes[1].runtime_minutes, Some(56));
        assert_eq!(season.episodes[1].air_date, Some(date("2011-04-24")));
    }

    #[test]
    fn series_rating_is_the_first_non_empty_per_country() {
        let r = |c: &str, v: &str| ContentRatingWire {
            iso_3166_1: c.into(),
            rating: v.into(),
        };
        let certs = series_certifications(vec![
            r("US", ""),
            r("US", "TV-MA"),
            r("US", "TV-14"),
            r("DE", "16 "),
        ]);
        assert_eq!(
            certs,
            vec![
                Certification {
                    country: "US".into(),
                    rating: "TV-MA".into()
                },
                Certification {
                    country: "DE".into(),
                    rating: "16".into()
                },
            ]
        );
    }

    #[test]
    fn trailer_prefers_official_youtube_trailers() {
        let v = |site: &str, kind: &str, key: &str, official: bool| VideoWire {
            site: site.into(),
            key: key.into(),
            name: key.into(),
            kind: kind.into(),
            official,
        };
        let t = trailer(vec![
            v("Vimeo", "Trailer", "aaaaaaaaaaa", true),
            v("YouTube", "Teaser", "bbbbbbbbbbb", true),
            v("YouTube", "Trailer", "ccccccccccc", false),
            v("YouTube", "Trailer", "ddddddddd-_", true),
        ]);
        assert_eq!(t.map(|t| t.key), Some("ddddddddd-_".to_owned()));
        let t = trailer(vec![v("YouTube", "Trailer", "ccccccccccc", false)]);
        assert_eq!(t.map(|t| t.key), Some("ccccccccccc".to_owned()));
        assert_eq!(
            trailer(vec![v("YouTube", "Clip", "xxxxxxxxxxx", true)]),
            None
        );
        // Keys that are not YouTube ids are never linked.
        assert_eq!(
            trailer(vec![v("YouTube", "Trailer", "javascript:", true)]),
            None
        );
    }

    #[test]
    fn specials_are_season_zero_flagged_and_sorted_first() {
        let s = seasons(vec![
            SeasonWire {
                season_number: 1,
                name: Some("Season 1".into()),
                episode_count: Some(10),
                air_date: Some("2020-01-01".into()),
                poster_path: None,
            },
            SeasonWire {
                season_number: 0,
                name: Some("Specials".into()),
                episode_count: Some(3),
                air_date: None,
                poster_path: Some(String::new()),
            },
        ]);
        assert_eq!(s[0].number, 0);
        assert!(s[0].is_special);
        assert_eq!(s[0].air_date, None);
        assert_eq!(s[0].poster, None);
        assert!(!s[1].is_special);
        assert_eq!(s[1].episode_count, 10);
    }

    #[test]
    fn providers_follow_the_region_priority() {
        let p = |id: u64, name: &str, prio: &[(&str, i64)]| ProviderWire {
            provider_id: id,
            provider_name: name.into(),
            logo_path: None,
            display_priorities: prio.iter().map(|(r, v)| ((*r).to_owned(), *v)).collect(),
        };
        let ordered = providers_in_order(
            vec![
                p(1, "Zeta", &[("CH", 1)]),
                p(2, "Alpha", &[("US", 0)]),
                p(3, "Beta", &[("CH", 0), ("US", 9)]),
            ],
            "CH",
        );
        let ids: Vec<u64> = ordered.iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![3, 1, 2]);
    }

    #[test]
    fn external_ids_skip_empty_values() {
        let ids = external_ids(
            7,
            Some(ExternalIdsWire {
                imdb_id: Some(String::new()),
                tvdb_id: Some(0),
            }),
        );
        assert_eq!(ids, vec![ExternalId::Tmdb(7)]);
        let ids = external_ids(
            7,
            Some(ExternalIdsWire {
                imdb_id: Some("tt1".into()),
                tvdb_id: Some(42),
            }),
        );
        assert_eq!(
            ids,
            vec![
                ExternalId::Tmdb(7),
                ExternalId::Tvdb(42),
                ExternalId::Imdb("tt1".into())
            ]
        );
    }
}
