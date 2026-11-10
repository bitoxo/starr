//! The `Source` capability (ADR 0009): services that acquire media for
//! Starr, such as Radarr and Sonarr (later Lidarr, Whisparr).
//!
//! Rules every implementation follows:
//! - items are addressed by their natural key, never by a stored remote ID
//!   alone (lesson 7);
//! - Starr only ever **adds**: it adds items and extends monitoring, but never
//!   deletes or changes the quality profile or folder of an item that is
//!   already in the service (spec 0003, D14). The one exception is
//!   [`Source::stop_monitoring`]: when nobody wants a title any more, Starr
//!   turns off exactly the monitoring it switched on itself (spec 0004 R1);
//! - every write is read back inside the adapter method and deviations are
//!   reported, not swallowed (lesson 9);
//! - lists carry completeness (lesson 8);
//! - outside the tested version range writes fail with
//!   [`AdapterError::VersionUnsupported`] while reads keep working (lesson 7).

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;

use uuid::Uuid;

use crate::adapters::{AdapterError, Listing, Probe};
use crate::categories::MinimumAvailability;

/// The natural key a source identifies an item by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NaturalKey {
    /// TMDB movie ID (Radarr).
    TmdbMovie(u32),
    /// TVDB series ID (Sonarr).
    TvdbSeries(u32),
    /// `MusicBrainz` artist MBID (Lidarr, spec 0006).
    MbArtist(Uuid),
    /// `StashDB` scene UUID, Whisparr v3's `foreignId` (spec 0007 Q15).
    /// Scenes have no units (read like a movie).
    StashScene(Uuid),
}

impl fmt::Display for NaturalKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TmdbMovie(id) => write!(f, "tmdb:{id}"),
            Self::TvdbSeries(id) => write!(f, "tvdb:{id}"),
            Self::MbArtist(id) => write!(f, "mbartist:{id}"),
            // Never printed with its UUID: a scene key names private content
            // (spec 0007, server logs).
            Self::StashScene(_) => f.write_str("stash:[private]"),
        }
    }
}

/// A child of a source item that is monitored, downloaded and counted on
/// its own: a season of a series or an album (release group) of an artist
/// (ADR 0013). A movie has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum UnitId {
    /// Season number; season 0 holds the specials.
    Season(u32),
    /// `MusicBrainz` release-group MBID.
    ReleaseGroup(Uuid),
}

impl UnitId {
    /// The season number, for seasons.
    #[must_use]
    pub const fn season(self) -> Option<u32> {
        match self {
            Self::Season(number) => Some(number),
            Self::ReleaseGroup(_) => None,
        }
    }

    /// Season 0 holds the specials; it never counts towards availability.
    #[must_use]
    pub const fn is_specials(self) -> bool {
        matches!(self, Self::Season(0))
    }
}

impl fmt::Display for UnitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Season(number) => write!(f, "season {number}"),
            Self::ReleaseGroup(id) => write!(f, "album {id}"),
        }
    }
}

/// A quality profile offered by the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityProfile {
    pub id: i64,
    pub name: String,
}

/// A metadata profile offered by the source (Lidarr): which release types
/// an artist keeps after a refresh (spec 0006 Q6). Radarr and Sonarr have
/// none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataProfile {
    pub id: i64,
    pub name: String,
    /// Allowed primary types as the source names them (`Album`, `EP`, …).
    pub primary_types: Vec<String>,
    /// Allowed secondary types (`Studio` stands for "none", `Live`, …).
    pub secondary_types: Vec<String>,
}

impl MetadataProfile {
    /// Whether the profile keeps plain albums and EPs, the release types
    /// Starr requests and follows by default (spec 0006 Q3 B): primary
    /// `Album` and `EP` and the secondary `Studio` (no secondary type).
    /// Otherwise a refresh may drop a requested album again (Q6 A warns).
    #[must_use]
    pub fn admits_albums_and_eps(&self) -> bool {
        let has = |list: &[String], name: &str| list.iter().any(|t| t.eq_ignore_ascii_case(name));
        has(&self.primary_types, "Album")
            && has(&self.primary_types, "EP")
            && has(&self.secondary_types, "Studio")
    }
}

/// A root folder new items can be placed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootFolder {
    pub id: i64,
    pub path: String,
    /// The source can currently reach the folder.
    pub accessible: bool,
    pub free_space_bytes: Option<u64>,
}

/// What to monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitorScope {
    /// The whole item (a movie).
    Item,
    /// These units: seasons of a series (season 0 holds the specials) or
    /// albums of an artist.
    Units(BTreeSet<UnitId>),
}

impl MonitorScope {
    /// These seasons of a series.
    pub fn seasons(numbers: impl IntoIterator<Item = u32>) -> Self {
        Self::Units(numbers.into_iter().map(UnitId::Season).collect())
    }
}

/// Add an item that is not in the source yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddRequest {
    pub key: NaturalKey,
    pub quality_profile_id: i64,
    pub root_folder_path: String,
    pub scope: MonitorScope,
    /// Ask the source to search for releases right away.
    pub search: bool,
    /// Movies: when the source may grab a release (D9, per category);
    /// `None` uses [`MinimumAvailability::default`]. Ignored for series.
    pub minimum_availability: Option<MinimumAvailability>,
    /// Music: the metadata profile a new artist gets (spec 0006 Q6);
    /// required by Lidarr, ignored by Radarr and Sonarr.
    pub metadata_profile_id: Option<i64>,
}

/// Monitoring of one unit (season, album) as the source reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitMonitoring {
    pub unit: UnitId,
    pub monitored: bool,
}

/// An item as it is in the source's library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryItem {
    /// The source's own ID; a cache, revalidated through `key` (lesson 7).
    pub remote_id: i64,
    pub key: NaturalKey,
    pub title: String,
    pub monitored: bool,
    pub quality_profile_id: i64,
    pub root_folder_path: Option<String>,
    /// Seasons of a series, albums of an artist; empty for movies.
    pub units: Vec<UnitMonitoring>,
}

impl LibraryItem {
    /// Whether `unit` exists and is monitored.
    #[must_use]
    pub fn unit_monitored(&self, unit: UnitId) -> bool {
        self.units.iter().any(|u| u.unit == unit && u.monitored)
    }
}

/// Result of looking an item up by its natural key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    NotInLibrary,
    InLibrary(LibraryItem),
}

/// A difference between what Starr wrote and what the read-back shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deviation {
    /// The item itself is not monitored.
    ItemNotMonitored,
    /// A unit that should be monitored is not (missing).
    UnitNotMonitored(UnitId),
    /// A unit is monitored that Starr did not ask for (unexpected extra),
    /// or one Starr stopped is still monitored.
    UnitUnexpectedlyMonitored(UnitId),
    /// The item is still monitored after Starr stopped it.
    ItemStillMonitored,
    QualityProfile {
        expected: i64,
        found: i64,
    },
    RootFolder {
        expected: String,
        found: Option<String>,
    },
}

/// What a write left behind, according to the read-back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteReport {
    /// The item as read back after the write.
    pub item: LibraryItem,
    /// `false` when nothing had to be written (already as requested).
    pub written: bool,
    /// Empty when the read-back matches the request.
    pub deviations: Vec<Deviation>,
}

/// Paths as the services print them, with or without a trailing separator.
fn same_path(a: &str, b: &str) -> bool {
    let trim = |p: &str| p.trim_end_matches(['/', '\\']).to_owned();
    trim(a) == trim(b)
}

fn check_common(
    item: &LibraryItem,
    quality_profile_id: i64,
    root_folder_path: Option<&str>,
    out: &mut Vec<Deviation>,
) {
    if !item.monitored {
        out.push(Deviation::ItemNotMonitored);
    }
    check_settings(item, quality_profile_id, root_folder_path, out);
}

/// Profile and folder as expected.
fn check_settings(
    item: &LibraryItem,
    quality_profile_id: i64,
    root_folder_path: Option<&str>,
    out: &mut Vec<Deviation>,
) {
    if item.quality_profile_id != quality_profile_id {
        out.push(Deviation::QualityProfile {
            expected: quality_profile_id,
            found: item.quality_profile_id,
        });
    }
    if let Some(expected) = root_folder_path
        && !item
            .root_folder_path
            .as_deref()
            .is_some_and(|found| same_path(found, expected))
    {
        out.push(Deviation::RootFolder {
            expected: expected.to_owned(),
            found: item.root_folder_path.clone(),
        });
    }
}

fn check_units(item: &LibraryItem, wanted: &BTreeSet<UnitId>, out: &mut Vec<Deviation>) {
    for &unit in wanted {
        if !item.unit_monitored(unit) {
            out.push(Deviation::UnitNotMonitored(unit));
        }
    }
    for u in &item.units {
        if u.monitored && !wanted.contains(&u.unit) {
            out.push(Deviation::UnitUnexpectedlyMonitored(u.unit));
        }
    }
}

/// Deviations of a freshly added item (read back) from the add request.
#[must_use]
pub fn check_added(request: &AddRequest, added: &LibraryItem) -> Vec<Deviation> {
    let mut out = Vec::new();
    check_common(
        added,
        request.quality_profile_id,
        Some(&request.root_folder_path),
        &mut out,
    );
    if let MonitorScope::Units(wanted) = &request.scope {
        check_units(added, wanted, &mut out);
    }
    out
}

/// Deviations after extending monitoring: everything monitored before plus
/// `scope` must be monitored, nothing else, and profile and folder must be
/// unchanged.
#[must_use]
pub fn check_extended(
    before: &LibraryItem,
    scope: &MonitorScope,
    after: &LibraryItem,
) -> Vec<Deviation> {
    let mut out = Vec::new();
    check_common(
        after,
        before.quality_profile_id,
        before.root_folder_path.as_deref(),
        &mut out,
    );
    if let MonitorScope::Units(requested) = scope {
        let mut wanted: BTreeSet<UnitId> = before
            .units
            .iter()
            .filter(|u| u.monitored)
            .map(|u| u.unit)
            .collect();
        wanted.extend(requested);
        check_units(after, &wanted, &mut out);
    }
    out
}

/// Deviations after stopping monitoring: a movie (`Item`) must no longer be
/// monitored; otherwise exactly the units in `scope` go off, every other
/// unit and the item itself stay as they were; profile and folder
/// are unchanged.
#[must_use]
pub fn check_stopped(
    before: &LibraryItem,
    scope: &MonitorScope,
    after: &LibraryItem,
) -> Vec<Deviation> {
    let mut out = Vec::new();
    match scope {
        MonitorScope::Item => {
            if after.monitored {
                out.push(Deviation::ItemStillMonitored);
            }
        }
        MonitorScope::Units(stopped) => {
            if before.monitored && !after.monitored {
                out.push(Deviation::ItemNotMonitored);
            }
            let wanted: BTreeSet<UnitId> = before
                .units
                .iter()
                .filter(|u| u.monitored && !stopped.contains(&u.unit))
                .map(|u| u.unit)
                .collect();
            check_units(after, &wanted, &mut out);
        }
    }
    check_settings(
        after,
        before.quality_profile_id,
        before.root_folder_path.as_deref(),
        &mut out,
    );
    out
}

/// Result of [`Source::add`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddOutcome {
    Added(WriteReport),
    /// The item was already in the library (or was added concurrently by
    /// someone else); it was left untouched. Use
    /// [`Source::extend_monitoring`] to monitor more of it. After a failed
    /// attempt (lost reply, failed read-back) a retry also lands here for
    /// Starr's own earlier add; the caller compares the item with its
    /// request ([`check_added`]) when that distinction matters.
    AlreadyExists(LibraryItem),
}

/// Rough state of a download in the source's queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueState {
    Queued,
    Paused,
    Downloading,
    /// Downloaded, waiting for or blocked before import.
    ImportPending,
    Importing,
    /// Needs attention (stalled, import blocked with a message, …).
    Warning,
    Failed,
    Other,
}

/// One entry of the source's download queue for an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueEntry {
    /// The unit the download belongs to (a season or an album).
    pub unit: Option<UnitId>,
    pub size_bytes: u64,
    pub size_left_bytes: u64,
    pub state: QueueState,
    /// The source's explanation for warnings and failures.
    pub message: Option<String>,
}

impl QueueEntry {
    /// Downloaded fraction in `0.0..=1.0`, if the size is known.
    #[must_use]
    pub fn fraction(&self) -> Option<f64> {
        if self.size_bytes == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)] // display value only
        let done =
            self.size_bytes.saturating_sub(self.size_left_bytes) as f64 / self.size_bytes as f64;
        Some(done.clamp(0.0, 1.0))
    }
}

/// Files of one unit: a season counted from the source's episode list, an
/// album from its track statistics (ADR 0013).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitProgress {
    pub unit: UnitId,
    pub monitored: bool,
    /// What should have a file by now: episodes whose air date has passed,
    /// tracks of the monitored release.
    pub expected: u32,
    /// Expected ones that have a file.
    pub expected_with_file: u32,
    /// Files, expected or not (a series may have a file for an episode
    /// that has not aired yet).
    pub with_file: u32,
}

/// How much of something has files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    None,
    Partial,
    /// Every aired episode (or the movie) has a file.
    Complete,
}

impl UnitProgress {
    /// Season 0 holds the specials; it never counts towards availability.
    #[must_use]
    pub const fn is_specials(&self) -> bool {
        self.unit.is_specials()
    }

    #[must_use]
    pub const fn state(&self) -> FileState {
        if self.expected > 0 && self.expected_with_file >= self.expected {
            FileState::Complete
        } else if self.with_file > 0 {
            FileState::Partial
        } else {
            FileState::None
        }
    }
}

/// File state of an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Files {
    Movie {
        has_file: bool,
    },
    /// Seasons of a series, albums of an artist.
    Units(Listing<UnitProgress>),
}

impl Files {
    /// Overall state of the **monitored** regular units (what was asked
    /// for); specials are ignored, and an incomplete unit list can never
    /// be [`FileState::Complete`] (lesson 8). Per-unit state is
    /// [`UnitProgress::state`].
    #[must_use]
    pub fn state(&self) -> FileState {
        match self {
            Self::Movie { has_file: true } => FileState::Complete,
            Self::Movie { has_file: false } => FileState::None,
            Self::Units(listing) => {
                let regular: Vec<_> = listing
                    .items
                    .iter()
                    .filter(|u| u.monitored && !u.is_specials())
                    .collect();
                let any_file = regular.iter().any(|u| u.with_file > 0);
                let due: Vec<_> = regular.iter().filter(|u| u.expected > 0).collect();
                let all_due_complete =
                    !due.is_empty() && due.iter().all(|u| u.state() == FileState::Complete);
                if listing.complete && all_due_complete {
                    FileState::Complete
                } else if any_file {
                    FileState::Partial
                } else {
                    FileState::None
                }
            }
        }
    }
}

/// Acquisition progress of one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub item: LibraryItem,
    pub files: Files,
    pub queue: Listing<QueueEntry>,
}

/// A service that acquires media (Radarr, Sonarr, …).
pub trait Source: Probe {
    /// Quality profiles to choose from per category (D9).
    fn quality_profiles(
        &self,
    ) -> impl Future<Output = Result<Listing<QualityProfile>, AdapterError>> + Send;

    /// Root folders to choose from per category (D9).
    fn root_folders(
        &self,
    ) -> impl Future<Output = Result<Listing<RootFolder>, AdapterError>> + Send;

    /// Metadata profiles to choose from per category (spec 0006 Q6). Only
    /// Lidarr has them; every other source answers with an empty, complete
    /// listing (ADR 0013).
    fn metadata_profiles(
        &self,
    ) -> impl Future<Output = Result<Listing<MetadataProfile>, AdapterError>> + Send {
        async {
            Ok(Listing {
                items: Vec::new(),
                complete: true,
            })
        }
    }

    /// Whether the item is in the library, and its state there (D14).
    fn lookup(&self, key: NaturalKey) -> impl Future<Output = Result<Lookup, AdapterError>> + Send;

    /// Add an item that is not in the library. Adds of the same key are
    /// serialized; an item that exists already (or appears concurrently) is
    /// left untouched and reported as [`AddOutcome::AlreadyExists`].
    fn add(
        &self,
        request: &AddRequest,
    ) -> impl Future<Output = Result<AddOutcome, AdapterError>> + Send;

    /// Monitor more of an item that is in the library: the item itself and
    /// the given units. Never unmonitors anything and never touches
    /// profile or folder. `search` asks the source to search for what was
    /// newly monitored. [`AdapterError::NotFound`] if the item is not in the
    /// library.
    fn extend_monitoring(
        &self,
        key: NaturalKey,
        scope: &MonitorScope,
        search: bool,
    ) -> impl Future<Output = Result<WriteReport, AdapterError>> + Send;

    /// Stop monitoring what Starr turned on and nobody wants any more (spec
    /// 0004 R1): the item itself (a movie) or the given units of an item
    /// (seasons of a series; the series stays monitored). The counterpart of
    /// [`Self::extend_monitoring`]: never deletes anything, never touches
    /// profile, folder, files, other units or the download queue, and
    /// searches nothing. The write is read back ([`check_stopped`]); with
    /// nothing to turn off nothing is written. [`AdapterError::NotFound`] if
    /// the item is not in the library.
    fn stop_monitoring(
        &self,
        key: NaturalKey,
        scope: &MonitorScope,
    ) -> impl Future<Output = Result<WriteReport, AdapterError>> + Send;

    /// Queue and file state of an item in the library.
    /// [`AdapterError::NotFound`] if the item is not in the library.
    fn progress(
        &self,
        key: NaturalKey,
    ) -> impl Future<Output = Result<Progress, AdapterError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn season(number: u32, aired: u32, aired_with_file: u32, with_file: u32) -> UnitProgress {
        UnitProgress {
            unit: UnitId::Season(number),
            monitored: true,
            expected: aired,
            expected_with_file: aired_with_file,
            with_file,
        }
    }

    fn seasons(items: Vec<UnitProgress>, complete: bool) -> Files {
        Files::Units(Listing { items, complete })
    }

    #[test]
    fn season_state_counts_aired_episodes_only() {
        assert_eq!(season(1, 10, 10, 10).state(), FileState::Complete);
        assert_eq!(season(1, 10, 3, 3).state(), FileState::Partial);
        assert_eq!(season(1, 10, 0, 0).state(), FileState::None);
        // Nothing aired yet: a file for a future episode is partial, not complete.
        assert_eq!(season(2, 0, 0, 1).state(), FileState::Partial);
        assert_eq!(season(2, 0, 0, 0).state(), FileState::None);
    }

    #[test]
    fn specials_never_count_for_the_series() {
        let files = seasons(vec![season(0, 5, 0, 0), season(1, 8, 8, 8)], true);
        assert_eq!(files.state(), FileState::Complete);
        let files = seasons(vec![season(0, 5, 5, 5), season(1, 8, 0, 0)], true);
        assert_eq!(files.state(), FileState::None);
    }

    #[test]
    fn unmonitored_seasons_do_not_count() {
        let mut old = season(1, 8, 0, 0);
        old.monitored = false;
        let files = seasons(vec![old, season(2, 8, 8, 8)], true);
        assert_eq!(files.state(), FileState::Complete);
    }

    #[test]
    fn unaired_seasons_do_not_block_completeness() {
        let files = seasons(vec![season(1, 8, 8, 8), season(2, 0, 0, 0)], true);
        assert_eq!(files.state(), FileState::Complete);
    }

    #[test]
    fn an_incomplete_listing_is_never_complete() {
        let files = seasons(vec![season(1, 8, 8, 8)], false);
        assert_eq!(files.state(), FileState::Partial);
    }

    #[test]
    fn queue_fraction() {
        let entry = |size, left| QueueEntry {
            unit: None,
            size_bytes: size,
            size_left_bytes: left,
            state: QueueState::Downloading,
            message: None,
        };
        assert_eq!(entry(100, 25).fraction(), Some(0.75));
        assert_eq!(entry(0, 0).fraction(), None);
        assert_eq!(entry(100, 200).fraction(), Some(0.0));
    }

    fn series(seasons: &[(u32, bool)]) -> LibraryItem {
        LibraryItem {
            remote_id: 7,
            key: NaturalKey::TvdbSeries(81189),
            title: "Breaking Bad".into(),
            monitored: true,
            quality_profile_id: 4,
            root_folder_path: Some("/tv/".into()),
            units: seasons
                .iter()
                .map(|&(number, monitored)| UnitMonitoring {
                    unit: UnitId::Season(number),
                    monitored,
                })
                .collect(),
        }
    }

    #[test]
    fn added_item_is_checked_against_the_request() {
        let request = AddRequest {
            key: NaturalKey::TvdbSeries(81189),
            quality_profile_id: 4,
            root_folder_path: "/tv".into(),
            scope: MonitorScope::seasons([1, 2]),
            search: false,
            minimum_availability: None,
            metadata_profile_id: None,
        };
        let exact = series(&[(0, false), (1, true), (2, true), (3, false)]);
        assert_eq!(check_added(&request, &exact), vec![]);

        let mut off = series(&[(0, true), (1, true), (2, false)]);
        off.quality_profile_id = 1;
        off.root_folder_path = None;
        off.monitored = false;
        assert_eq!(
            check_added(&request, &off),
            vec![
                Deviation::ItemNotMonitored,
                Deviation::QualityProfile {
                    expected: 4,
                    found: 1
                },
                Deviation::RootFolder {
                    expected: "/tv".into(),
                    found: None
                },
                Deviation::UnitNotMonitored(UnitId::Season(2)),
                Deviation::UnitUnexpectedlyMonitored(UnitId::Season(0)),
            ]
        );
    }

    #[test]
    fn extending_keeps_what_was_monitored_and_adds_the_scope() {
        let before = series(&[(1, true), (2, false), (3, false)]);
        let scope = MonitorScope::seasons([2]);
        let after = series(&[(1, true), (2, true), (3, false)]);
        assert_eq!(check_extended(&before, &scope, &after), vec![]);

        // Lost season 1, gained season 3, profile changed: all reported.
        let mut wrong = series(&[(1, false), (2, true), (3, true)]);
        wrong.quality_profile_id = 9;
        assert_eq!(
            check_extended(&before, &scope, &wrong),
            vec![
                Deviation::QualityProfile {
                    expected: 4,
                    found: 9
                },
                Deviation::UnitNotMonitored(UnitId::Season(1)),
                Deviation::UnitUnexpectedlyMonitored(UnitId::Season(3)),
            ]
        );
    }

    #[test]
    fn stopping_turns_off_exactly_the_scope() {
        let before = series(&[(1, true), (2, true), (3, false)]);
        let scope = MonitorScope::seasons([2]);
        let after = series(&[(1, true), (2, false), (3, false)]);
        assert_eq!(check_stopped(&before, &scope, &after), vec![]);

        // Season 2 still on, season 1 lost, series switched off: reported.
        let mut wrong = series(&[(1, false), (2, true), (3, false)]);
        wrong.monitored = false;
        assert_eq!(
            check_stopped(&before, &scope, &wrong),
            vec![
                Deviation::ItemNotMonitored,
                Deviation::UnitNotMonitored(UnitId::Season(1)),
                Deviation::UnitUnexpectedlyMonitored(UnitId::Season(2)),
            ]
        );

        let movie = |monitored| LibraryItem {
            remote_id: 1,
            key: NaturalKey::TmdbMovie(603),
            title: "The Matrix".into(),
            monitored,
            quality_profile_id: 4,
            root_folder_path: Some("/movies/".into()),
            units: Vec::new(),
        };
        assert_eq!(
            check_stopped(&movie(true), &MonitorScope::Item, &movie(false)),
            vec![]
        );
        assert_eq!(
            check_stopped(&movie(true), &MonitorScope::Item, &movie(true)),
            vec![Deviation::ItemStillMonitored]
        );
    }

    #[test]
    fn metadata_profiles_must_keep_albums_and_eps() {
        let profile = |primary: &[&str], secondary: &[&str]| MetadataProfile {
            id: 1,
            name: "Standard".into(),
            primary_types: primary.iter().map(|&t| t.to_owned()).collect(),
            secondary_types: secondary.iter().map(|&t| t.to_owned()).collect(),
        };
        assert!(profile(&["Album", "EP", "Single"], &["Studio", "Live"]).admits_albums_and_eps());
        assert!(profile(&["album", "ep"], &["studio"]).admits_albums_and_eps());
        assert!(!profile(&["Album"], &["Studio"]).admits_albums_and_eps());
        assert!(!profile(&["Album", "EP"], &["Live"]).admits_albums_and_eps());
        // Lidarr's "None" profile allows nothing.
        assert!(!profile(&[], &[]).admits_albums_and_eps());
    }

    #[test]
    fn keys_display_with_their_scheme() {
        assert_eq!(NaturalKey::TmdbMovie(603).to_string(), "tmdb:603");
        assert_eq!(NaturalKey::TvdbSeries(81189).to_string(), "tvdb:81189");
        let mbid = Uuid::from_u128(0x5b11_f4ce_a62d_471e_81fc_a69a_8278_c7da);
        assert_eq!(
            NaturalKey::MbArtist(mbid).to_string(),
            "mbartist:5b11f4ce-a62d-471e-81fc-a69a8278c7da"
        );
    }

    const ALBUM: Uuid = Uuid::from_u128(0x1b02_2e01_4da6_387b_8658_8678_046e_4cef);

    #[test]
    fn units_name_themselves_and_only_season_zero_is_specials() {
        assert_eq!(UnitId::Season(2).to_string(), "season 2");
        assert_eq!(
            UnitId::ReleaseGroup(ALBUM).to_string(),
            "album 1b022e01-4da6-387b-8658-8678046e4cef"
        );
        assert!(UnitId::Season(0).is_specials());
        assert!(!UnitId::Season(1).is_specials());
        assert!(!UnitId::ReleaseGroup(Uuid::nil()).is_specials());
        assert_eq!(UnitId::Season(3).season(), Some(3));
        assert_eq!(UnitId::ReleaseGroup(ALBUM).season(), None);
    }

    #[test]
    fn an_album_is_complete_when_every_track_has_a_file() {
        let album = |tracks, with_file| UnitProgress {
            unit: UnitId::ReleaseGroup(ALBUM),
            monitored: true,
            expected: tracks,
            expected_with_file: with_file,
            with_file,
        };
        assert_eq!(album(12, 12).state(), FileState::Complete);
        assert_eq!(album(12, 5).state(), FileState::Partial);
        assert_eq!(album(12, 0).state(), FileState::None);
        let files = Files::Units(Listing {
            items: vec![album(12, 12)],
            complete: true,
        });
        assert_eq!(files.state(), FileState::Complete);
    }

    #[test]
    fn album_units_are_checked_like_seasons() {
        let mut before = series(&[]);
        before.key = NaturalKey::MbArtist(Uuid::nil());
        let mut after = before.clone();
        after.units.push(UnitMonitoring {
            unit: UnitId::ReleaseGroup(ALBUM),
            monitored: true,
        });
        let scope = MonitorScope::Units([UnitId::ReleaseGroup(ALBUM)].into());
        assert_eq!(check_extended(&before, &scope, &after), vec![]);
        assert_eq!(
            check_stopped(&after, &scope, &after),
            vec![Deviation::UnitUnexpectedlyMonitored(UnitId::ReleaseGroup(
                ALBUM
            ))]
        );
    }
}
