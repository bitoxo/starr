//! The `Library` capability (ADR 0009): read what a consume service (Jellyfin,
//! Emby, Navidrome; later Audiobookshelf) has in its libraries, and build
//! "open in" links to its web client.
//!
//! Rules every implementation follows:
//! - read only: nothing here writes to the service (spec 0003, O3);
//! - items carry the service's provider IDs, which are the join to Starr's
//!   catalog (natural keys, lesson 7); the service's own IDs are a cache;
//! - every list carries completeness evidence (lesson 8): `complete` only
//!   when the service reported a total and Starr received exactly that many
//!   distinct items with the total unchanged across pages.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;

use uuid::Uuid;

use crate::adapters::{AdapterError, Listing, Probe};

/// What a library holds, as far as Starr cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LibraryContent {
    Movies,
    Series,
    Music,
    Books,
    /// Mixed, home videos, collections, playlists, … (not synced).
    Other,
}

/// One library of the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaLibrary {
    /// The service's ID of the library. Not durable: Jellyfin assigns a new
    /// one when a library is renamed (`capabilities.md`).
    pub id: String,
    /// The ID account grants name the library by (spec 0005 M4): Emby's
    /// library `Guid`, Jellyfin's `ItemId` (then equal to `id`). `None` when
    /// the service did not report it: the library is still read for the
    /// inventory, but cannot be mapped or granted. Item queries always take
    /// `id`.
    pub grant_id: Option<String>,
    pub name: String,
    pub content: LibraryContent,
}

/// What kind of thing an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ItemKind {
    Movie,
    Series,
    Season,
    Episode,
    /// A music album (spec 0006); keyed by its release-group MBID.
    Album,
}

impl ItemKind {
    /// Stable name used in the database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Series => "series",
            Self::Season => "season",
            Self::Episode => "episode",
            Self::Album => "album",
        }
    }
}

/// IDs of an item in public databases, as the service reports them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderIds {
    /// TMDB movie ID for movies, TMDB TV ID for series.
    pub tmdb: Option<u32>,
    pub tvdb: Option<u32>,
    /// `tt…`.
    pub imdb: Option<String>,
    /// `MusicBrainz` release-group MBID (albums).
    pub mb_release_group: Option<Uuid>,
    /// `MusicBrainz` MBID of the (album) artist.
    pub mb_artist: Option<Uuid>,
}

impl ProviderIds {
    /// Read provider IDs from the service's `name → value` pairs. Names are
    /// compared case-insensitively (Emby writes `IMDB` for series and
    /// `Imdb` for movies); values that are empty or not of the expected
    /// shape are ignored, unknown providers too.
    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut ids = Self::default();
        let mut album_artist = None;
        for (name, value) in pairs {
            let value = value.trim();
            match name.to_ascii_lowercase().as_str() {
                "tmdb" => ids.tmdb = ids.tmdb.or_else(|| positive(value)),
                "tvdb" => ids.tvdb = ids.tvdb.or_else(|| positive(value)),
                "imdb" if is_imdb_id(value) => {
                    ids.imdb = ids.imdb.take().or_else(|| Some(value.to_owned()));
                }
                // Jellyfin/Emby provider names (Jellyfin `MetadataProvider`).
                "musicbrainzreleasegroup" => {
                    ids.mb_release_group = ids.mb_release_group.or_else(|| mbid(value));
                }
                "musicbrainzalbumartist" => album_artist = album_artist.or_else(|| mbid(value)),
                "musicbrainzartist" => ids.mb_artist = ids.mb_artist.or_else(|| mbid(value)),
                _ => {}
            }
        }
        // The album artist wins over a track artist.
        ids.mb_artist = album_artist.or(ids.mb_artist);
        ids
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tmdb.is_none()
            && self.tvdb.is_none()
            && self.imdb.is_none()
            && self.mb_release_group.is_none()
            && self.mb_artist.is_none()
    }
}

fn positive(value: &str) -> Option<u32> {
    value.parse().ok().filter(|id| *id > 0)
}

/// A `MusicBrainz` id: a UUID that is not nil.
fn mbid(value: &str) -> Option<Uuid> {
    Uuid::try_parse(value).ok().filter(|id| !id.is_nil())
}

fn is_imdb_id(value: &str) -> bool {
    value
        .strip_prefix("tt")
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Episodes of one season that are present (have a file) in the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeasonPresence {
    /// Season 0 holds the specials.
    pub number: u32,
    /// Distinct episode numbers present.
    pub episodes: u32,
}

/// What of an item is present, counted in its units (ADR 0013: the season
/// counts generalised). On an incomplete listing the counts may be too low
/// (positive evidence only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitCounts {
    /// A movie: the item itself, no smaller unit.
    Whole,
    /// A series: present episodes per season.
    Seasons(Vec<SeasonPresence>),
    /// An album: tracks present (the service's song count).
    Tracks(u32),
}

impl UnitCounts {
    /// Present episodes per season (empty for other kinds).
    #[must_use]
    pub fn seasons(&self) -> &[SeasonPresence] {
        match self {
            Self::Seasons(seasons) => seasons,
            Self::Whole | Self::Tracks(_) => &[],
        }
    }

    /// Tracks present, for albums.
    #[must_use]
    pub const fn tracks(&self) -> Option<u32> {
        match self {
            Self::Tracks(n) => Some(*n),
            Self::Whole | Self::Seasons(_) => None,
        }
    }
}

/// One item of a library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaItem {
    /// The service's ID of the item (for links); a cache, the provider IDs
    /// are the identity.
    pub remote_id: String,
    pub kind: ItemKind,
    pub provider_ids: ProviderIds,
    pub title: String,
    pub year: Option<u32>,
    /// What of it is present: episodes per season, or an album's tracks.
    pub units: UnitCounts,
}

/// The natural key Starr keeps inventory under: the provider ID that
/// Starr's sources key the kind by (Radarr: TMDB movie, Sonarr: TVDB
/// series), falling back to TMDB TV and `IMDb`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InventoryKey(String);

impl InventoryKey {
    /// `None` for items without a usable provider ID, and for seasons and
    /// episodes (they are tracked inside their series).
    #[must_use]
    pub fn of(kind: ItemKind, ids: &ProviderIds) -> Option<Self> {
        let key = match kind {
            ItemKind::Movie => ids
                .tmdb
                .map(|id| format!("movie:tmdb:{id}"))
                .or_else(|| ids.imdb.as_ref().map(|id| format!("movie:imdb:{id}"))),
            ItemKind::Series => ids
                .tvdb
                .map(|id| format!("series:tvdb:{id}"))
                .or_else(|| ids.tmdb.map(|id| format!("series:tmdb:{id}")))
                .or_else(|| ids.imdb.as_ref().map(|id| format!("series:imdb:{id}"))),
            ItemKind::Album => ids.mb_release_group.map(|id| format!("album:mbrg:{id}")),
            ItemKind::Season | ItemKind::Episode => None,
        };
        key.map(Self)
    }

    /// A key read back from storage.
    #[must_use]
    pub fn from_stored(key: String) -> Self {
        Self(key)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for InventoryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Present episodes per season as a map, merging duplicates by maximum.
#[must_use]
pub fn seasons_map(seasons: &[SeasonPresence]) -> BTreeMap<u32, u32> {
    let mut map = BTreeMap::new();
    for s in seasons {
        let entry = map.entry(s.number).or_insert(0);
        *entry = (*entry).max(s.episodes);
    }
    map
}

/// Where an "open in" link points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemLink<'a> {
    /// The address members reach the service's web client at (the
    /// instance's public URL; never its internal base URL), without trailing
    /// slash.
    pub public_url: &'a str,
    /// The service's server ID, if it has one (Jellyfin, Emby).
    pub server_id: Option<&'a str>,
    pub remote_id: &'a str,
}

/// A consume service whose libraries Starr reads.
pub trait Library: Probe {
    /// The service's stable server ID, if it has one; part of the links.
    fn server_id(&self) -> impl Future<Output = Result<Option<String>, AdapterError>> + Send;

    /// All libraries of the service.
    fn libraries(&self)
    -> impl Future<Output = Result<Listing<MediaLibrary>, AdapterError>> + Send;

    /// Movies and series (seasons as counts inside their series) or albums
    /// (tracks as counts) of one library. `complete` only with proof.
    fn items(
        &self,
        library_id: &str,
    ) -> impl Future<Output = Result<Listing<MediaItem>, AdapterError>> + Send;

    /// URL of the item's page in the service's web client. Pure: the web
    /// process builds links from stored data without talking to the
    /// service.
    fn item_url(link: &ItemLink<'_>) -> String
    where
        Self: Sized;
}

/// Percent-encode a value for a URL query or fragment component (RFC 3986
/// unreserved characters stay as they are).
#[must_use]
pub fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0xF)]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_are_read_case_insensitively() {
        let ids = ProviderIds::from_pairs([
            ("Tmdb", "603"),
            ("IMDB", "tt0133093"),
            ("tvdb", " 81189 "),
            ("TvRage", "1"),
        ]);
        assert_eq!(
            ids,
            ProviderIds {
                tmdb: Some(603),
                tvdb: Some(81189),
                imdb: Some("tt0133093".into()),
                ..ProviderIds::default()
            }
        );
    }

    #[test]
    fn malformed_provider_ids_are_ignored() {
        let ids = ProviderIds::from_pairs([
            ("Tmdb", ""),
            ("Tvdb", "0"),
            ("Imdb", "0133093"),
            ("imdb", "tt"),
            ("Tmdb", "abc"),
        ]);
        assert!(ids.is_empty(), "{ids:?}");
        // The first usable value wins over later duplicates.
        let ids = ProviderIds::from_pairs([("Tmdb", "x"), ("tmdb", "7"), ("TMDB", "8")]);
        assert_eq!(ids.tmdb, Some(7));
    }

    #[test]
    fn inventory_keys_prefer_what_sources_key_by() {
        let all = ProviderIds {
            tmdb: Some(1399),
            tvdb: Some(121_361),
            imdb: Some("tt0944947".into()),
            ..ProviderIds::default()
        };
        let key = |kind, ids: &ProviderIds| InventoryKey::of(kind, ids).map(|k| k.to_string());
        assert_eq!(
            key(ItemKind::Series, &all).as_deref(),
            Some("series:tvdb:121361")
        );
        assert_eq!(
            key(ItemKind::Movie, &all).as_deref(),
            Some("movie:tmdb:1399")
        );
        let tmdb_only = ProviderIds {
            tvdb: None,
            ..all.clone()
        };
        assert_eq!(
            key(ItemKind::Series, &tmdb_only).as_deref(),
            Some("series:tmdb:1399")
        );
        let imdb_only = ProviderIds {
            imdb: Some("tt1".into()),
            ..ProviderIds::default()
        };
        assert_eq!(
            key(ItemKind::Movie, &imdb_only).as_deref(),
            Some("movie:imdb:tt1")
        );
        assert_eq!(key(ItemKind::Movie, &ProviderIds::default()), None);
        assert_eq!(key(ItemKind::Episode, &all), None);
    }

    const RELEASE_GROUP: &str = "1b022e01-4da6-387b-8658-8678046e4cef";
    const ARTIST: &str = "5b11f4ce-a62d-471e-81fc-a69a8278c7da";
    const OTHER_ARTIST: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";

    fn uuid(s: &str) -> Uuid {
        Uuid::parse_str(s).unwrap()
    }

    #[test]
    fn music_provider_ids_are_read_and_album_artists_win() {
        let ids = ProviderIds::from_pairs([
            ("MusicBrainzArtist", OTHER_ARTIST),
            ("MusicBrainzReleaseGroup", RELEASE_GROUP),
            ("MusicBrainzAlbumArtist", ARTIST),
            ("MusicBrainzAlbum", "00000000-0000-0000-0000-000000000001"),
        ]);
        assert_eq!(ids.mb_release_group, Some(uuid(RELEASE_GROUP)));
        assert_eq!(ids.mb_artist, Some(uuid(ARTIST)));
        assert!(!ids.is_empty());

        let ids = ProviderIds::from_pairs([("musicbrainzartist", OTHER_ARTIST)]);
        assert_eq!(ids.mb_artist, Some(uuid(OTHER_ARTIST)));

        let ids = ProviderIds::from_pairs([
            ("MusicBrainzReleaseGroup", "not-a-uuid"),
            (
                "MusicBrainzReleaseGroup",
                "00000000-0000-0000-0000-000000000000",
            ),
            ("MusicBrainzAlbumArtist", ""),
        ]);
        assert!(ids.is_empty(), "{ids:?}");
    }

    #[test]
    fn albums_are_keyed_by_release_group() {
        let ids = ProviderIds {
            mb_release_group: Some(uuid(RELEASE_GROUP)),
            mb_artist: Some(uuid(ARTIST)),
            tmdb: Some(603),
            ..ProviderIds::default()
        };
        assert_eq!(
            InventoryKey::of(ItemKind::Album, &ids).map(|k| k.to_string()),
            Some(format!("album:mbrg:{RELEASE_GROUP}"))
        );
        let artist_only = ProviderIds {
            mb_artist: Some(uuid(ARTIST)),
            ..ProviderIds::default()
        };
        assert_eq!(InventoryKey::of(ItemKind::Album, &artist_only), None);
        // Video kinds never key by music ids.
        assert_eq!(InventoryKey::of(ItemKind::Movie, &artist_only), None);
        assert_eq!(ItemKind::Album.as_str(), "album");
    }

    #[test]
    fn seasons_merge_by_maximum() {
        let map = seasons_map(&[
            SeasonPresence {
                number: 1,
                episodes: 3,
            },
            SeasonPresence {
                number: 1,
                episodes: 5,
            },
            SeasonPresence {
                number: 0,
                episodes: 1,
            },
        ]);
        assert_eq!(map, BTreeMap::from([(0, 1), (1, 5)]));
    }

    #[test]
    fn components_are_percent_encoded() {
        assert_eq!(encode_component("a1-._~"), "a1-._~");
        assert_eq!(encode_component("a b&c=d#"), "a%20b%26c%3Dd%23");
    }
}
