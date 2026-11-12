//! Follows (domain model §6, spec 0003 D8, spec 0006 Q3): a member's
//! personal "new seasons of this series" or "new albums of this artist".
//! Pure rules; storage lives in `starr-db`, the daily check
//! (`follows.check`) in `starr-worker`.
//!
//! A follow starts at a cutoff: the regular seasons that have aired when
//! the member starts following are known, everything after them is new
//! (maintainer, 2026-11-02: an announced season airing later is what one
//! follows for). A season is new once the catalog lists it with an air
//! date (D8), so an announced season without a date is not new yet.
//! Specials never count.
//!
//! An artist follow starts at a date, the day following started (ADR 0012
//! `known_through`): a release group is new when its first release is
//! after it and its types are the category's (spec 0006 Q3 B: album and EP
//! without secondary types by default). A partial date counts as the first
//! day of its period (the catalog reads it so); a release group without a
//! date is never new, and becomes new once it has one after the cutoff.
//!
//! Performer and studio follows (spec 0007 Q4) start at a date the same
//! way: a scene is new when its release date is after the follow date.
//! They request at most a weekly cap of scenes per follow (an admin
//! setting of the private category, default 10): counted over a rolling
//! week of the requests the follow itself created, oldest scenes first;
//! the rest stay new for a later run or the member's own request.

use chrono::{DateTime, NaiveDate, TimeDelta, Utc};

use crate::catalog::{CatalogEntry, Relevance, Season};

/// The window the follow cap counts over: a rolling week of the follow's
/// own automatic requests (spec 0007 Q4 addendum).
pub const CAP_WINDOW: TimeDelta = TimeDelta::days(7);

/// Automatic requests per follow and week until the admin sets another
/// number (spec 0007 Q4, admin story 12).
pub const DEFAULT_CAP_PER_WEEK: u32 = 10;

/// The weekly caps an admin may set.
pub const CAP_RANGE: std::ops::RangeInclusive<u32> = 1..=100;

/// What one `follows.check` run does with a follow's wanted scenes under
/// the weekly cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapSplit {
    /// How many to request now (the oldest first).
    pub request: usize,
    /// How many stay new beyond the cap: the hint "N more new scenes this
    /// week" and one `follow_capped` notification a week.
    pub over: usize,
}

/// Split `wanted` scenes by the weekly `cap` when the follow created
/// `used` automatic requests in the last [`CAP_WINDOW`]. Requests the
/// member made by hand are not part of `used`.
#[must_use]
pub fn split_by_cap(wanted: usize, used: u32, cap: u32) -> CapSplit {
    let room = usize::try_from(cap.saturating_sub(used)).unwrap_or(usize::MAX);
    let request = wanted.min(room);
    CapSplit {
        request,
        over: wanted - request,
    }
}

/// Which week of the follow `now` is in, counted from when it started: a
/// capped follow notifies once per such week.
#[must_use]
pub fn cap_week(followed_at: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    (now - followed_at).num_days().max(0) / CAP_WINDOW.num_days()
}

/// The scenes of a performer or studio that are new for a follow with
/// date cutoff `known_through`: released after it (a partial date counts
/// as its first day, as the catalog reads it; undated never), oldest
/// first, so the cap requests the earliest ones (ties by id).
#[must_use]
pub fn new_scenes<'a>(
    scenes: impl IntoIterator<Item = &'a CatalogEntry>,
    known_through: NaiveDate,
) -> Vec<&'a CatalogEntry> {
    let mut new: Vec<&CatalogEntry> = scenes
        .into_iter()
        .filter(|s| s.release_date.is_some_and(|d| d > known_through))
        .collect();
    new.sort_by(|a, b| {
        a.release_date
            .cmp(&b.release_date)
            .then_with(|| a.item.id.cmp(&b.item.id))
    });
    new.dedup_by(|a, b| a.item.id == b.item.id);
    new
}

/// How often each follow is checked. `follows.check` runs every hour and
/// checks the follows whose last check is at least this long ago, so the
/// catalog reads spread over the day by when each follow started (spec
/// 0006 Q14, ADR 0013: no synchronized polling of `MusicBrainz`). A full
/// day also outlasts the catalog's one-day discography cache, so every
/// check reads what the catalog lists now.
pub const CHECK_INTERVAL: TimeDelta = TimeDelta::hours(24);

/// Whether `season` counts for follows: a regular season with an air date.
const fn counts(season: &Season) -> bool {
    !season.is_special && season.number > 0 && season.air_date.is_some()
}

/// The cutoff of a follow starting on `today`: the highest regular season
/// that has aired (air date on or before `today`), `0` if none has.
#[must_use]
pub fn seasons_through<'a>(seasons: impl IntoIterator<Item = &'a Season>, today: NaiveDate) -> u32 {
    seasons
        .into_iter()
        .filter(|s| counts(s) && s.air_date.is_some_and(|d| d <= today))
        .map(|s| s.number)
        .max()
        .unwrap_or(0)
}

/// The seasons that are new for a follow with cutoff `through`: regular
/// seasons with an air date after it, in ascending order.
#[must_use]
pub fn new_seasons<'a>(seasons: impl IntoIterator<Item = &'a Season>, through: u32) -> Vec<u32> {
    let mut new: Vec<u32> = seasons
        .into_iter()
        .filter(|s| counts(s) && s.number > through)
        .map(|s| s.number)
        .collect();
    new.sort_unstable();
    new.dedup();
    new
}

/// The albums of an artist's discography that are new for a follow with
/// date cutoff `known_through`: first released after it, of the types
/// `relevance` admits. Newest last, as found.
#[must_use]
pub fn new_albums<'a>(
    albums: impl IntoIterator<Item = &'a CatalogEntry>,
    known_through: NaiveDate,
    relevance: &Relevance,
) -> Vec<&'a CatalogEntry> {
    albums
        .into_iter()
        .filter(|a| a.release_date.is_some_and(|d| d > known_through))
        .filter(|a| {
            a.album
                .as_ref()
                .is_some_and(|f| relevance.admits_album(f.primary_type, &f.secondary_types))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;
    use crate::catalog::{AlbumFacts, CatalogRef, MediaKind, ReleaseType, SecondaryType};

    fn album(
        id: &str,
        primary: ReleaseType,
        secondary: &[SecondaryType],
        released: Option<(i32, u32, u32)>,
    ) -> CatalogEntry {
        CatalogEntry {
            item: CatalogRef {
                kind: MediaKind::Album,
                id: id.to_owned(),
            },
            title: id.to_owned(),
            original_title: id.to_owned(),
            original_language: String::new(),
            overview: String::new(),
            release_date: released.and_then(|(y, m, d)| NaiveDate::from_ymd_opt(y, m, d)),
            poster: None,
            backdrop: None,
            genre_ids: Vec::new(),
            rating: None,
            vote_count: 0,
            album: Some(AlbumFacts {
                artist_credit: "Artist".to_owned(),
                artist: None,
                primary_type: Some(primary),
                secondary_types: secondary.to_vec(),
                track_count: None,
                ..AlbumFacts::default()
            }),
            scene: None,
        }
    }

    fn ids(found: &[&CatalogEntry]) -> Vec<String> {
        found.iter().map(|a| a.item.id.clone()).collect()
    }

    fn season(number: u32, aired: Option<(i32, u32, u32)>) -> Season {
        Season {
            number,
            name: format!("Season {number}"),
            episode_count: 10,
            air_date: aired.and_then(|(y, m, d)| NaiveDate::from_ymd_opt(y, m, d)),
            is_special: number == 0,
            poster: None,
            episodes: Vec::new(),
        }
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 2).unwrap_or_default()
    }

    #[test]
    fn the_cutoff_is_the_last_season_that_aired() {
        let seasons = [
            season(0, Some((2010, 12, 5))),
            season(1, Some((2011, 4, 17))),
            season(2, Some((2012, 4, 1))),
            season(9, None),
        ];
        assert_eq!(seasons_through(&seasons, today()), 2);
        assert_eq!(seasons_through(&[season(1, None)], today()), 0);
        assert_eq!(seasons_through(&[], today()), 0);
    }

    #[test]
    fn an_announced_season_with_a_future_air_date_is_followed() {
        // Maintainer, 2026-11-02: only aired seasons are known when the
        // follow starts; season 3 airs later and gets asked for.
        let seasons = [
            season(1, Some((2025, 1, 17))),
            season(2, Some((2026, 11, 2))),
            season(3, Some((2027, 1, 1))),
        ];
        let through = seasons_through(&seasons, today());
        assert_eq!(through, 2, "a season airing today has aired");
        assert_eq!(new_seasons(&seasons, through), vec![3]);
    }

    #[test]
    fn a_season_is_new_once_it_has_an_air_date_after_the_cutoff() {
        let announced = [season(1, Some((2011, 4, 17))), season(3, None)];
        assert_eq!(new_seasons(&announced, 1), Vec::<u32>::new());
        let dated = [
            season(3, Some((2027, 1, 1))),
            season(1, Some((2011, 4, 17))),
            season(2, Some((2026, 6, 1))),
        ];
        assert_eq!(new_seasons(&dated, 1), vec![2, 3]);
        assert_eq!(new_seasons(&dated, 3), Vec::<u32>::new());
    }

    #[test]
    fn specials_never_count() {
        let mut special = season(4, Some((2026, 12, 24)));
        special.is_special = true;
        let seasons = [season(0, Some((2026, 1, 1))), special];
        assert_eq!(seasons_through(&seasons, today()), 0);
        assert_eq!(new_seasons(&seasons, 0), Vec::<u32>::new());
    }

    #[test]
    fn an_album_is_new_when_first_released_after_the_follow_date() {
        let albums = [
            album("before", ReleaseType::Album, &[], Some((2026, 6, 1))),
            album("on_the_day", ReleaseType::Album, &[], Some((2026, 11, 2))),
            album("after", ReleaseType::Album, &[], Some((2026, 11, 20))),
            album("announced", ReleaseType::Ep, &[], Some((2027, 3, 1))),
        ];
        let found = new_albums(&albums, today(), &Relevance::music_default());
        assert_eq!(ids(&found), ["after", "announced"]);
    }

    #[test]
    fn only_the_categorys_release_types_count() {
        // Spec 0006 acceptance: a new album, a live album and a single
        // after the follow date make exactly one request.
        let albums = [
            album("album", ReleaseType::Album, &[], Some((2026, 12, 1))),
            album(
                "live",
                ReleaseType::Album,
                &[SecondaryType::Live],
                Some((2026, 12, 1)),
            ),
            album("single", ReleaseType::Single, &[], Some((2026, 12, 1))),
            album(
                "compilation",
                ReleaseType::Album,
                &[SecondaryType::Compilation],
                Some((2026, 12, 1)),
            ),
        ];
        let found = new_albums(&albums, today(), &Relevance::music_default());
        assert_eq!(ids(&found), ["album"]);

        // The setting is the category's: with singles admitted they count.
        let mut relevance = Relevance::music_default();
        relevance.release_types.push(ReleaseType::Single);
        let found = new_albums(&albums, today(), &relevance);
        assert_eq!(ids(&found), ["album", "single"]);
    }

    #[test]
    fn partial_dates_count_as_their_first_day_and_undated_never_count() {
        // The catalog reads `2027` as 2027-01-01 and `2026-11` as
        // 2026-11-01: a partial date in the follow's month is not after it.
        let albums = [
            album("year", ReleaseType::Album, &[], Some((2027, 1, 1))),
            album("month", ReleaseType::Album, &[], Some((2026, 11, 1))),
            album("undated", ReleaseType::Album, &[], None),
        ];
        let found = new_albums(&albums, today(), &Relevance::music_default());
        assert_eq!(ids(&found), ["year"]);
    }

    fn scene(id: &str, released: Option<(i32, u32, u32)>) -> CatalogEntry {
        let mut entry = album(id, ReleaseType::Album, &[], released);
        entry.item.kind = MediaKind::Scene;
        entry.album = None;
        entry
    }

    #[test]
    fn scenes_after_the_follow_date_are_new_oldest_first() {
        // Spec 0007 Q4: released after the follow date; partial dates are
        // their first day, undated scenes never count.
        let scenes = [
            scene("late", Some((2026, 11, 20))),
            scene("before", Some((2026, 10, 1))),
            scene("on_the_day", Some((2026, 11, 2))),
            scene("month", Some((2026, 11, 1))),
            scene("undated", None),
            scene("early", Some((2026, 11, 3))),
            scene("b_same_day", Some((2026, 11, 5))),
            scene("a_same_day", Some((2026, 11, 5))),
            scene("early", Some((2026, 11, 3))),
        ];
        let found = new_scenes(&scenes, today());
        assert_eq!(ids(&found), ["early", "a_same_day", "b_same_day", "late"]);
    }

    #[test]
    fn the_cap_requests_up_to_the_room_left_this_week() {
        // 14 new with cap 10 and nothing yet this week: 10 now, 4 over.
        assert_eq!(
            split_by_cap(14, 0, 10),
            CapSplit {
                request: 10,
                over: 4
            }
        );
        // The same follow a day later: the week is used up.
        assert_eq!(
            split_by_cap(4, 10, 10),
            CapSplit {
                request: 0,
                over: 4
            }
        );
        // A lowered cap below what was used asks for nothing.
        assert_eq!(
            split_by_cap(3, 10, 5),
            CapSplit {
                request: 0,
                over: 3
            }
        );
        assert_eq!(
            split_by_cap(3, 2, 10),
            CapSplit {
                request: 3,
                over: 0
            }
        );
        assert_eq!(
            split_by_cap(0, 0, 10),
            CapSplit {
                request: 0,
                over: 0
            }
        );
        assert!(CAP_RANGE.contains(&DEFAULT_CAP_PER_WEEK));
    }

    #[test]
    fn a_follow_notifies_about_its_cap_once_per_week() {
        let start = DateTime::parse_from_rfc3339("2026-11-01T12:00:00Z")
            .map_or_default(|d| d.with_timezone(&Utc));
        assert_eq!(cap_week(start, start), 0);
        assert_eq!(cap_week(start, start + TimeDelta::days(6)), 0);
        assert_eq!(cap_week(start, start + TimeDelta::days(7)), 1);
        assert_eq!(cap_week(start, start + TimeDelta::days(15)), 2);
        // A clock behind the follow counts as its first week.
        assert_eq!(cap_week(start, start - TimeDelta::days(1)), 0);
    }

    #[test]
    fn an_album_without_types_is_not_new() {
        let mut untyped = album("untyped", ReleaseType::Album, &[], Some((2027, 1, 1)));
        untyped.album = None;
        let found = new_albums([&untyped], today(), &Relevance::music_default());
        assert_eq!(ids(&found), Vec::<String>::new());
    }
}
