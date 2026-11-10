//! `MusicBrainz` `/ws/2` (JSON) and `ListenBrainz` response shapes and their
//! mapping to the core catalog types. Only the fields Starr reads are
//! declared; required ones are not optional, so a changed shape fails
//! loudly as `Unknown` (lesson 10). Unknown fields are ignored.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use starr_core::catalog::{
    AlbumFacts, CatalogEntry, CatalogRef, MediaKind, ReleaseType, SecondaryType, Track,
};
use uuid::Uuid;

/// An MBID in its canonical spelling (lower-case, hyphenated), or `None`
/// for anything that is not a UUID.
pub fn mbid(raw: &str) -> Option<Uuid> {
    Uuid::parse_str(raw.trim()).ok()
}

/// The canonical text of an MBID, as Starr's API and image proxy use it.
pub fn mbid_text(id: Uuid) -> String {
    id.hyphenated().to_string()
}

/// `MusicBrainz` dates may be partial: `2026` counts as 2026-01-01 and
/// `2026-11` as 2026-11-01 (spec 0006 Q3); empty or malformed is `None`.
pub fn partial_date(raw: Option<&str>) -> Option<NaiveDate> {
    let raw = raw?.trim();
    let mut parts = raw.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u32 = match parts.next() {
        Some(m) => m.parse().ok()?,
        None => 1,
    };
    let day: u32 = match parts.next() {
        Some(d) => d.get(..2).unwrap_or(d).parse().ok()?,
        None => 1,
    };
    if raw.len() < 4 {
        return None;
    }
    NaiveDate::from_ymd_opt(year, month, day)
}

/// A primary type as `MusicBrainz` and `ListenBrainz` spell it.
pub fn release_type(raw: Option<&str>) -> Option<ReleaseType> {
    let raw = raw?.trim();
    ReleaseType::ALL
        .into_iter()
        .find(|t| t.as_str().eq_ignore_ascii_case(raw))
}

/// A secondary type as `MusicBrainz` spells it (`Audio drama`, `DJ-mix`,
/// `Mixtape/Street`, `Field recording`); unknown names are dropped.
pub fn secondary_type(raw: &str) -> Option<SecondaryType> {
    let key: String = raw
        .trim()
        .chars()
        .take_while(|c| *c != '/')
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    SecondaryType::ALL
        .into_iter()
        .find(|t| t.as_str().replace('_', "") == key)
}

/// `count` / `offset` of a search or browse page.
pub fn has_more(offset: u64, len: usize, count: u64) -> bool {
    offset.saturating_add(len as u64) < count
}

/// `GET /ws/2/artist?query=`.
#[derive(Debug, Deserialize)]
pub struct ArtistSearchWire {
    pub count: u64,
    pub offset: u64,
    pub artists: Vec<ArtistWire>,
}

/// An artist (search result or lookup).
#[derive(Debug, Deserialize)]
pub struct ArtistWire {
    pub id: String,
    pub name: String,
    #[serde(rename = "sort-name", default)]
    pub sort_name: Option<String>,
    #[serde(default)]
    pub disambiguation: Option<String>,
    /// With `inc=genres` (lookup only).
    #[serde(default)]
    pub genres: Vec<GenreWire>,
}

/// A genre with its vote count (`inc=genres`). Genres are tags:
/// supplementary data under CC BY-NC-SA (spec 0008 Q17 B).
#[derive(Debug, Deserialize)]
pub struct GenreWire {
    pub name: String,
    #[serde(default)]
    pub count: i64,
}

/// Genres to show at most per album or artist.
const GENRE_LIMIT: usize = 5;

/// Genre names, most voted first (ties by name), at most [`GENRE_LIMIT`].
pub fn genre_names(mut genres: Vec<GenreWire>) -> Vec<String> {
    genres.retain(|g| !g.name.trim().is_empty());
    genres.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    genres
        .into_iter()
        .take(GENRE_LIMIT)
        .map(|g| g.name.trim().to_owned())
        .collect()
}

impl ArtistWire {
    pub fn into_entry(self) -> Option<CatalogEntry> {
        let id = mbid(&self.id)?;
        Some(CatalogEntry {
            item: CatalogRef {
                kind: MediaKind::Artist,
                id: mbid_text(id),
            },
            original_title: self.sort_name.unwrap_or_else(|| self.name.clone()),
            title: self.name,
            original_language: String::new(),
            overview: self.disambiguation.unwrap_or_default(),
            release_date: None,
            poster: None,
            backdrop: None,
            genre_ids: Vec::new(),
            rating: None,
            vote_count: 0,
            album: None,
            scene: None,
        })
    }
}

/// `GET /ws/2/release-group?query=`.
#[derive(Debug, Deserialize)]
pub struct ReleaseGroupSearchWire {
    pub count: u64,
    pub offset: u64,
    #[serde(rename = "release-groups")]
    pub release_groups: Vec<ReleaseGroupWire>,
}

/// A release-group search kept as sent (a genre lane), so the worker can
/// rank it and store it in the shape [`ReleaseGroupSearchWire`] reads.
#[derive(Debug, Deserialize, Serialize)]
pub struct RawSearchWire {
    pub count: u64,
    pub offset: u64,
    #[serde(rename = "release-groups")]
    pub release_groups: Vec<serde_json::Value>,
}

impl RawSearchWire {
    /// Most listened first (ties keep the search order); release groups
    /// without listens are dropped.
    pub fn rank_by(&mut self, listens: &std::collections::HashMap<String, u64>) {
        let count = |rg: &serde_json::Value| {
            rg.get("id")
                .and_then(serde_json::Value::as_str)
                .and_then(|id| listens.get(id))
                .copied()
                .unwrap_or(0)
        };
        self.release_groups.retain(|rg| count(rg) > 0);
        self.release_groups
            .sort_by_cached_key(|rg| std::cmp::Reverse(count(rg)));
        self.count = self.release_groups.len() as u64;
    }
}

/// One entry of `POST /1/popularity/release-group` (`ListenBrainz`, seen
/// live 2026-11-07): counts are `null` or 0 when unknown.
#[derive(Debug, Deserialize)]
pub struct PopularityWire {
    /// Required: an answer without it has changed shape and fails loudly.
    pub release_group_mbid: String,
    #[serde(default)]
    pub total_listen_count: Option<u64>,
}

/// `GET /ws/2/release-group?artist=` (browse).
#[derive(Debug, Deserialize)]
pub struct ReleaseGroupBrowseWire {
    #[serde(rename = "release-group-count")]
    pub count: u64,
    #[serde(rename = "release-group-offset")]
    pub offset: u64,
    #[serde(rename = "release-groups")]
    pub release_groups: Vec<ReleaseGroupWire>,
}

/// A release group (album): search result, browse result or lookup.
#[derive(Debug, Deserialize)]
pub struct ReleaseGroupWire {
    pub id: String,
    pub title: String,
    #[serde(rename = "primary-type", default)]
    pub primary_type: Option<String>,
    #[serde(rename = "secondary-types", default)]
    pub secondary_types: Vec<String>,
    #[serde(rename = "first-release-date", default)]
    pub first_release_date: Option<String>,
    #[serde(default)]
    pub disambiguation: Option<String>,
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<CreditWire>,
    #[serde(default)]
    pub releases: Vec<ReleaseRefWire>,
    /// With `inc=genres` (lookup only).
    #[serde(default)]
    pub genres: Vec<GenreWire>,
}

/// One credited artist: the name as credited, then the join phrase
/// (`" & "`) to the next one.
#[derive(Debug, Deserialize)]
pub struct CreditWire {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub joinphrase: Option<String>,
    pub artist: CreditArtistWire,
}

#[derive(Debug, Deserialize)]
pub struct CreditArtistWire {
    pub id: String,
    pub name: String,
}

/// A release of a release group, as listed with `inc=releases`.
#[derive(Debug, Deserialize)]
pub struct ReleaseRefWire {
    pub id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
}

/// `GET /ws/2/release/<mbid>?inc=media+recordings+artist-credits+labels`.
#[derive(Debug, Deserialize)]
pub struct ReleaseWire {
    #[serde(default)]
    pub media: Vec<MediumWire>,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(rename = "label-info", default)]
    pub label_info: Vec<LabelInfoWire>,
}

#[derive(Debug, Deserialize)]
pub struct MediumWire {
    #[serde(rename = "track-count", default)]
    pub track_count: u32,
    #[serde(default)]
    pub position: Option<u32>,
    #[serde(default)]
    pub title: Option<String>,
    /// With `inc=recordings`.
    #[serde(default)]
    pub tracks: Vec<TrackWire>,
}

/// A track of a medium; `length` in milliseconds.
#[derive(Debug, Deserialize)]
pub struct TrackWire {
    #[serde(default)]
    pub position: Option<u32>,
    pub title: String,
    #[serde(default)]
    pub length: Option<u64>,
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<CreditWire>,
}

#[derive(Debug, Deserialize)]
pub struct LabelInfoWire {
    #[serde(default)]
    pub label: Option<LabelWire>,
}

#[derive(Debug, Deserialize)]
pub struct LabelWire {
    pub name: String,
}

impl ReleaseWire {
    /// The tracklist in disc and track order; a track's artist credit only
    /// where it differs from `album_credit`.
    pub fn tracks(&self, album_credit: &str) -> Vec<Track> {
        let mut out = Vec::new();
        for (index, medium) in self.media.iter().enumerate() {
            let disc = medium
                .position
                .unwrap_or_else(|| u32::try_from(index + 1).unwrap_or(u32::MAX));
            for (i, track) in medium.tracks.iter().enumerate() {
                let credit = credit_text(&track.artist_credit);
                out.push(Track {
                    disc,
                    disc_title: medium.title.clone().unwrap_or_default(),
                    position: track
                        .position
                        .unwrap_or_else(|| u32::try_from(i + 1).unwrap_or(u32::MAX)),
                    title: track.title.clone(),
                    length_seconds: track
                        .length
                        .and_then(|ms| u32::try_from(ms.div_ceil(1000)).ok())
                        .filter(|s| *s > 0),
                    artist_credit: (!credit.is_empty() && credit != album_credit).then_some(credit),
                });
            }
        }
        out.sort_by_key(|t| (t.disc, t.position));
        out
    }

    /// The first named label.
    pub fn label(&self) -> Option<String> {
        self.label_info
            .iter()
            .filter_map(|l| l.label.as_ref())
            .map(|l| l.name.trim())
            .find(|n| !n.is_empty())
            .map(str::to_owned)
    }
}

/// The credit as printed: credited names joined by their join phrases.
pub fn credit_text(credits: &[CreditWire]) -> String {
    let mut out = String::new();
    for c in credits {
        out.push_str(c.name.as_deref().unwrap_or(&c.artist.name));
        out.push_str(c.joinphrase.as_deref().unwrap_or_default());
    }
    out.trim().to_owned()
}

impl ReleaseGroupWire {
    /// The album entry; `credit` overrides the release group's own artist
    /// credit (browse results carry none: the artist is known).
    pub fn into_entry(self, credit: Option<(&str, Uuid)>) -> Option<CatalogEntry> {
        let id = mbid(&self.id)?;
        let (artist_credit, artist) = match credit {
            Some((name, artist)) => (name.to_owned(), Some(artist)),
            None => (
                credit_text(&self.artist_credit),
                self.artist_credit.first().and_then(|c| mbid(&c.artist.id)),
            ),
        };
        Some(CatalogEntry {
            item: CatalogRef {
                kind: MediaKind::Album,
                id: mbid_text(id),
            },
            original_title: self.title.clone(),
            title: self.title,
            original_language: String::new(),
            overview: self.disambiguation.unwrap_or_default(),
            release_date: partial_date(self.first_release_date.as_deref()),
            poster: Some(crate::cover_ref(id)),
            backdrop: None,
            genre_ids: Vec::new(),
            rating: None,
            vote_count: 0,
            album: Some(AlbumFacts {
                artist_credit,
                artist,
                primary_type: release_type(self.primary_type.as_deref()),
                secondary_types: self
                    .secondary_types
                    .iter()
                    .filter_map(|s| secondary_type(s))
                    .collect(),
                track_count: None,
                ..AlbumFacts::default()
            }),
            scene: None,
        })
    }
}

/// The release a release group's track count is read from: the earliest
/// official release, else the earliest of any status, else the first.
pub fn counted_release(releases: &[ReleaseRefWire]) -> Option<&ReleaseRefWire> {
    let date = |r: &ReleaseRefWire| partial_date(r.date.as_deref()).unwrap_or(NaiveDate::MAX);
    let official = |r: &&ReleaseRefWire| {
        r.status
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case("official"))
    };
    releases
        .iter()
        .filter(official)
        .min_by_key(|r| date(r))
        .or_else(|| releases.iter().min_by_key(|r| date(r)))
}

/// `GET /1/stats/sitewide/release-groups` (`ListenBrainz`). A 204 (not
/// computed yet) arrives as `None`.
#[derive(Debug, Deserialize)]
pub struct StatsWire {
    pub payload: StatsPayloadWire,
}

#[derive(Debug, Deserialize)]
pub struct StatsPayloadWire {
    pub offset: u64,
    pub total_release_group_count: u64,
    pub release_groups: Vec<StatsReleaseGroupWire>,
}

/// A release group in a `ListenBrainz` statistic; `release_group_mbid` and
/// `artist_mbids` "may not be present in all the responses".
#[derive(Debug, Deserialize)]
pub struct StatsReleaseGroupWire {
    #[serde(default)]
    pub artist_mbids: Vec<String>,
    pub artist_name: String,
    #[serde(default)]
    pub release_group_mbid: Option<String>,
    pub release_group_name: String,
}

impl StatsReleaseGroupWire {
    /// The album entry; `None` without a release-group MBID.
    pub fn into_entry(self) -> Option<CatalogEntry> {
        let id = mbid(self.release_group_mbid.as_deref()?)?;
        Some(lb_entry(
            id,
            self.release_group_name,
            self.artist_name,
            self.artist_mbids.first().and_then(|a| mbid(a)),
            None,
            None,
            Vec::new(),
        ))
    }
}

/// `GET /1/explore/fresh-releases/` (`ListenBrainz`).
#[derive(Debug, Deserialize)]
pub struct FreshWire {
    pub payload: FreshPayloadWire,
}

#[derive(Debug, Deserialize)]
pub struct FreshPayloadWire {
    pub releases: Vec<FreshReleaseWire>,
}

#[derive(Debug, Deserialize)]
pub struct FreshReleaseWire {
    pub artist_credit_name: String,
    #[serde(default)]
    pub artist_mbids: Vec<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub release_group_mbid: Option<String>,
    #[serde(default)]
    pub release_group_primary_type: Option<String>,
    #[serde(default)]
    pub release_group_secondary_type: Option<String>,
    pub release_name: String,
}

impl FreshReleaseWire {
    /// The album entry; `None` without a release-group MBID.
    pub fn into_entry(self) -> Option<CatalogEntry> {
        let id = mbid(self.release_group_mbid.as_deref()?)?;
        Some(lb_entry(
            id,
            self.release_name,
            self.artist_credit_name,
            self.artist_mbids.first().and_then(|a| mbid(a)),
            partial_date(self.release_date.as_deref()),
            release_type(self.release_group_primary_type.as_deref()),
            self.release_group_secondary_type
                .as_deref()
                .and_then(secondary_type)
                .into_iter()
                .collect(),
        ))
    }
}

fn lb_entry(
    id: Uuid,
    title: String,
    artist_credit: String,
    artist: Option<Uuid>,
    release_date: Option<NaiveDate>,
    primary_type: Option<ReleaseType>,
    secondary_types: Vec<SecondaryType>,
) -> CatalogEntry {
    CatalogEntry {
        item: CatalogRef {
            kind: MediaKind::Album,
            id: mbid_text(id),
        },
        original_title: title.clone(),
        title,
        original_language: String::new(),
        overview: String::new(),
        release_date,
        poster: Some(crate::cover_ref(id)),
        backdrop: None,
        genre_ids: Vec::new(),
        rating: None,
        vote_count: 0,
        album: Some(AlbumFacts {
            artist_credit,
            artist,
            primary_type,
            secondary_types,
            track_count: None,
            ..AlbumFacts::default()
        }),
        scene: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_dates_count_as_their_first_day() {
        let d = |y, m, d| NaiveDate::from_ymd_opt(y, m, d);
        assert_eq!(partial_date(Some("2026")), d(2026, 1, 1));
        assert_eq!(partial_date(Some("2026-11")), d(2026, 11, 1));
        assert_eq!(partial_date(Some("2026-11-03")), d(2026, 11, 3));
        for bad in ["", "26", "2026-13", "2026-02-30", "x", "2026-xx"] {
            assert_eq!(partial_date(Some(bad)), None, "{bad:?}");
        }
        assert_eq!(partial_date(None), None);
    }

    #[test]
    fn types_are_read_as_musicbrainz_spells_them() {
        assert_eq!(release_type(Some("Album")), Some(ReleaseType::Album));
        assert_eq!(release_type(Some("EP")), Some(ReleaseType::Ep));
        assert_eq!(release_type(Some("Thing")), None);
        assert_eq!(
            secondary_type("Audio drama"),
            Some(SecondaryType::AudioDrama)
        );
        assert_eq!(secondary_type("DJ-mix"), Some(SecondaryType::DjMix));
        assert_eq!(
            secondary_type("Mixtape/Street"),
            Some(SecondaryType::Mixtape)
        );
        assert_eq!(
            secondary_type("Field recording"),
            Some(SecondaryType::FieldRecording)
        );
        assert_eq!(secondary_type("Live"), Some(SecondaryType::Live));
        assert_eq!(
            secondary_type("Spokenword"),
            Some(SecondaryType::Spokenword)
        );
        assert_eq!(secondary_type("Bootleg"), None);
    }

    #[test]
    fn mbids_are_canonical() {
        let id = mbid(" A74B1B7F-71A5-4011-9441-D0B5E4122711").unwrap();
        assert_eq!(mbid_text(id), "a74b1b7f-71a5-4011-9441-d0b5e4122711");
        assert_eq!(mbid("../etc"), None);
    }

    #[test]
    fn the_earliest_official_release_is_counted() {
        let r = |id: &str, status: Option<&str>, date: Option<&str>| ReleaseRefWire {
            id: id.into(),
            status: status.map(Into::into),
            date: date.map(Into::into),
        };
        let releases = [
            r("bootleg", Some("Bootleg"), Some("1990")),
            r("late", Some("Official"), Some("2001-05-01")),
            r("early", Some("Official"), Some("2000")),
        ];
        assert_eq!(counted_release(&releases).unwrap().id, "early");
        let unofficial = [r("b", None, Some("1999")), r("a", None, None)];
        assert_eq!(counted_release(&unofficial).unwrap().id, "b");
        assert!(counted_release(&[]).is_none());
    }
}
