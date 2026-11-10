//! Inventory planning (domain model §5, lesson 8): from what a service
//! listed now and what Starr knew before, decide per natural key whether an
//! item appeared, is still there, may be missing, or is gone.
//!
//! "Gone" is a statement only with proof: the item was absent from at least
//! [`AbsencePolicy::complete_runs`] **complete** listings in a row, and last
//! seen at least [`AbsencePolicy::min_duration`] ago. An incomplete listing
//! never counts towards absence; it only adds positive evidence. Nothing is
//! deleted: gone items keep their row with `gone_since`.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, TimeDelta, Utc};

use crate::adapters::Listing;
use crate::library::InventoryKey;

/// Present episodes per season number.
pub type Seasons = BTreeMap<u32, u32>;

/// When an absent item counts as gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsencePolicy {
    /// Complete listings in a row that must miss the item (>= 1).
    pub complete_runs: u32,
    /// Minimum time since the item was last seen.
    pub min_duration: TimeDelta,
}

impl AbsencePolicy {
    /// Lesson 8: two complete runs plus a minimum duration.
    pub const DEFAULT: Self = Self {
        complete_runs: 2,
        min_duration: TimeDelta::hours(6),
    };
}

/// What Starr knows about one item in one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: InventoryKey,
    /// Since when the item is (continuously) present; `None` while gone.
    pub present_since: Option<DateTime<Utc>>,
    pub last_seen: DateTime<Utc>,
    /// Complete listings in a row that did not contain the item.
    pub missing_runs: u32,
    /// When the item was declared gone; `None` while present.
    pub gone_since: Option<DateTime<Utc>>,
    pub seasons: Seasons,
    /// Albums: tracks present; `None` for other kinds and while gone.
    pub tracks: Option<u32>,
}

/// One item as listed now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub key: InventoryKey,
    pub seasons: Seasons,
    /// Albums: tracks present.
    pub tracks: Option<u32>,
}

/// What happened to an entry in this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// First time Starr sees the item in this service.
    Appeared,
    /// Seen again after it was gone.
    Returned,
    /// Seen, and it was present before.
    StillPresent,
    /// Missing from a complete listing, but not long enough to be gone.
    Unconfirmed,
    /// Absence proven: gone from now on.
    Gone,
}

/// The new state of one entry and how it got there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub entry: Entry,
    pub transition: Transition,
}

/// The counts of one key as listed in this run.
#[derive(Debug, Clone, Default)]
struct Seen {
    seasons: Seasons,
    tracks: Option<u32>,
}

impl Seen {
    /// Merge `seasons` and `tracks` in by maximum.
    fn merge(&mut self, seasons: &Seasons, tracks: Option<u32>) {
        merge_max(&mut self.seasons, seasons);
        self.tracks = match (self.tracks, tracks) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
}

/// Plan one run. Returns a decision for every entry that changes; entries
/// that stay as they are (gone and still not listed, or present and not
/// listed by an incomplete listing) get none.
#[must_use]
pub fn plan(
    previous: &[Entry],
    listing: &Listing<Observation>,
    now: DateTime<Utc>,
    policy: AbsencePolicy,
) -> Vec<Decision> {
    // A key listed twice (same title in two libraries) merges by maximum.
    let merged: HashMap<&InventoryKey, Seen> =
        listing.items.iter().fold(HashMap::new(), |mut acc, item| {
            acc.entry(&item.key)
                .or_default()
                .merge(&item.seasons, item.tracks);
            acc
        });

    let mut decisions = Vec::new();
    let known: HashMap<&InventoryKey, &Entry> = previous.iter().map(|e| (&e.key, e)).collect();

    for (key, seen) in &merged {
        let fresh = |transition| Decision {
            entry: Entry {
                key: (*key).clone(),
                present_since: Some(now),
                last_seen: now,
                missing_runs: 0,
                gone_since: None,
                seasons: seen.seasons.clone(),
                tracks: seen.tracks,
            },
            transition,
        };
        let decision = match known.get(key) {
            None => fresh(Transition::Appeared),
            Some(before) if before.gone_since.is_some() || before.present_since.is_none() => {
                fresh(Transition::Returned)
            }
            Some(before) => {
                let counts = if listing.complete {
                    seen.clone()
                } else {
                    // Fewer episodes or tracks in an incomplete listing
                    // prove nothing.
                    let mut counts = Seen {
                        seasons: before.seasons.clone(),
                        tracks: before.tracks,
                    };
                    counts.merge(&seen.seasons, seen.tracks);
                    counts
                };
                Decision {
                    entry: Entry {
                        key: (*key).clone(),
                        present_since: before.present_since,
                        last_seen: now,
                        missing_runs: 0,
                        gone_since: None,
                        seasons: counts.seasons,
                        tracks: counts.tracks,
                    },
                    transition: Transition::StillPresent,
                }
            }
        };
        decisions.push(decision);
    }

    if listing.complete {
        for before in previous {
            let present = before.gone_since.is_none() && before.present_since.is_some();
            if !present || merged.contains_key(&before.key) {
                continue;
            }
            let missing_runs = before.missing_runs.saturating_add(1);
            let gone = missing_runs >= policy.complete_runs.max(1)
                && now - before.last_seen >= policy.min_duration;
            decisions.push(if gone {
                Decision {
                    entry: Entry {
                        present_since: None,
                        missing_runs,
                        gone_since: Some(now),
                        seasons: Seasons::new(),
                        tracks: None,
                        ..before.clone()
                    },
                    transition: Transition::Gone,
                }
            } else {
                Decision {
                    entry: Entry {
                        missing_runs,
                        ..before.clone()
                    },
                    transition: Transition::Unconfirmed,
                }
            });
        }
    }
    decisions.sort_by(|a, b| a.entry.key.cmp(&b.entry.key));
    decisions
}

fn merge_max(into: &mut Seasons, from: &Seasons) {
    for (&number, &episodes) in from {
        let slot = into.entry(number).or_insert(0);
        *slot = (*slot).max(episodes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(hours: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap() + TimeDelta::hours(hours)
    }

    fn key(s: &str) -> InventoryKey {
        InventoryKey::from_stored(s.to_owned())
    }

    fn obs(k: &str, seasons: &[(u32, u32)]) -> Observation {
        Observation {
            key: key(k),
            seasons: seasons.iter().copied().collect(),
            tracks: None,
        }
    }

    fn album(k: &str, tracks: u32) -> Observation {
        Observation {
            key: key(k),
            seasons: Seasons::new(),
            tracks: Some(tracks),
        }
    }

    fn listing(items: Vec<Observation>, complete: bool) -> Listing<Observation> {
        Listing { items, complete }
    }

    fn present(k: &str, since: i64, last_seen: i64) -> Entry {
        Entry {
            key: key(k),
            present_since: Some(t(since)),
            last_seen: t(last_seen),
            missing_runs: 0,
            gone_since: None,
            seasons: Seasons::new(),
            tracks: None,
        }
    }

    const POLICY: AbsencePolicy = AbsencePolicy::DEFAULT;

    /// Apply decisions to a state, like the job does when persisting.
    fn step(state: &mut Vec<Entry>, listing: &Listing<Observation>, at: DateTime<Utc>) {
        let decisions = plan(state, listing, at, POLICY);
        apply(state, decisions);
    }

    fn apply(state: &mut Vec<Entry>, decisions: Vec<Decision>) {
        for d in decisions {
            match state.iter_mut().find(|e| e.key == d.entry.key) {
                Some(e) => *e = d.entry,
                None => state.push(d.entry),
            }
        }
    }

    fn transitions(decisions: &[Decision]) -> Vec<(&str, Transition)> {
        decisions
            .iter()
            .map(|d| (d.entry.key.as_str(), d.transition))
            .collect()
    }

    #[test]
    fn new_items_appear_and_known_ones_stay_present() {
        let prev = vec![present("movie:tmdb:1", 0, 0)];
        let d = plan(
            &prev,
            &listing(
                vec![obs("movie:tmdb:1", &[]), obs("movie:tmdb:2", &[])],
                true,
            ),
            t(1),
            POLICY,
        );
        assert_eq!(
            transitions(&d),
            vec![
                ("movie:tmdb:1", Transition::StillPresent),
                ("movie:tmdb:2", Transition::Appeared)
            ]
        );
        assert_eq!(d[0].entry.present_since, Some(t(0)), "since is kept");
        assert_eq!(d[0].entry.last_seen, t(1));
        assert_eq!(d[1].entry.present_since, Some(t(1)));
    }

    #[test]
    fn an_incomplete_listing_never_yields_absence() {
        let prev = vec![present("movie:tmdb:1", 0, 0)];
        for hours in [1, 100, 10_000] {
            let d = plan(&prev, &listing(vec![], false), t(hours), POLICY);
            assert_eq!(d, vec![], "after {hours}h");
        }
        // Even many incomplete runs in a row change nothing.
        let mut state = prev.clone();
        for run in 1..10 {
            let d = plan(&state, &listing(vec![], false), t(run * 24), POLICY);
            apply(&mut state, d);
        }
        assert_eq!(state, prev);
    }

    #[test]
    fn absence_needs_two_complete_runs() {
        let mut state = vec![present("movie:tmdb:1", 0, 0)];
        let d = plan(&state, &listing(vec![], true), t(24), POLICY);
        assert_eq!(
            transitions(&d),
            vec![("movie:tmdb:1", Transition::Unconfirmed)]
        );
        assert_eq!(d[0].entry.missing_runs, 1);
        assert_eq!(d[0].entry.present_since, Some(t(0)), "still present");
        apply(&mut state, d);
        let d = plan(&state, &listing(vec![], true), t(25), POLICY);
        assert_eq!(transitions(&d), vec![("movie:tmdb:1", Transition::Gone)]);
        assert_eq!(d[0].entry.gone_since, Some(t(25)));
        assert_eq!(d[0].entry.present_since, None);
        assert_eq!(d[0].entry.last_seen, t(0), "last seen is kept");
    }

    #[test]
    fn absence_needs_the_minimum_duration_since_last_seen() {
        let mut state = vec![present("movie:tmdb:1", 0, 0)];
        // Two quick complete runs within the minimum duration: not gone.
        for hours in [1, 2, 3, 5] {
            let d = plan(&state, &listing(vec![], true), t(hours), POLICY);
            assert_eq!(d[0].transition, Transition::Unconfirmed, "at {hours}h");
            apply(&mut state, d);
        }
        assert_eq!(state[0].missing_runs, 4);
        // Exactly the minimum duration: gone.
        let d = plan(&state, &listing(vec![], true), t(6), POLICY);
        assert_eq!(d[0].transition, Transition::Gone);
    }

    #[test]
    fn an_incomplete_run_between_complete_ones_neither_counts_nor_resets() {
        let mut state = vec![present("movie:tmdb:1", 0, 0)];
        step(&mut state, &listing(vec![], true), t(10));
        step(&mut state, &listing(vec![], false), t(11));
        assert_eq!(state[0].missing_runs, 1);
        let d = plan(&state, &listing(vec![], true), t(12), POLICY);
        assert_eq!(d[0].transition, Transition::Gone);
    }

    #[test]
    fn seeing_an_item_resets_the_missing_count_even_from_an_incomplete_listing() {
        let mut state = vec![present("movie:tmdb:1", 0, 0)];
        step(&mut state, &listing(vec![], true), t(10));
        assert_eq!(state[0].missing_runs, 1);
        let seen = listing(vec![obs("movie:tmdb:1", &[])], false);
        step(&mut state, &seen, t(11));
        assert_eq!(state[0].missing_runs, 0);
        assert_eq!(state[0].last_seen, t(11));
        // One complete miss afterwards is not enough again.
        let d = plan(&state, &listing(vec![], true), t(30), POLICY);
        assert_eq!(d[0].transition, Transition::Unconfirmed);
    }

    #[test]
    fn gone_items_return_with_a_new_since_and_stay_gone_silently() {
        let gone = Entry {
            present_since: None,
            gone_since: Some(t(5)),
            missing_runs: 2,
            ..present("movie:tmdb:1", 0, 0)
        };
        assert_eq!(
            plan(
                std::slice::from_ref(&gone),
                &listing(vec![], true),
                t(50),
                POLICY
            ),
            vec![],
            "no decision for an item that stays gone"
        );
        let d = plan(
            &[gone],
            &listing(vec![obs("movie:tmdb:1", &[])], false),
            t(50),
            POLICY,
        );
        assert_eq!(
            transitions(&d),
            vec![("movie:tmdb:1", Transition::Returned)]
        );
        assert_eq!(d[0].entry.present_since, Some(t(50)));
        assert_eq!(d[0].entry.gone_since, None);
        assert_eq!(d[0].entry.missing_runs, 0);
    }

    #[test]
    fn seasons_are_replaced_by_complete_listings_and_only_grow_from_incomplete_ones() {
        let mut before = present("series:tvdb:1", 0, 0);
        before.seasons = Seasons::from([(1, 10), (2, 4)]);
        let partial = listing(vec![obs("series:tvdb:1", &[(1, 3), (2, 6)])], false);
        let d = plan(&[before.clone()], &partial, t(1), POLICY);
        assert_eq!(d[0].entry.seasons, Seasons::from([(1, 10), (2, 6)]));

        let full = listing(vec![obs("series:tvdb:1", &[(1, 9)])], true);
        let d = plan(&[before], &full, t(1), POLICY);
        assert_eq!(
            d[0].entry.seasons,
            Seasons::from([(1, 9)]),
            "a complete listing is authoritative, also when episodes went"
        );
    }

    #[test]
    fn duplicate_keys_merge_their_seasons() {
        let d = plan(
            &[],
            &listing(
                vec![
                    obs("series:tvdb:1", &[(1, 2), (2, 8)]),
                    obs("series:tvdb:1", &[(1, 5)]),
                ],
                true,
            ),
            t(0),
            POLICY,
        );
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].entry.seasons, Seasons::from([(1, 5), (2, 8)]));
    }

    #[test]
    fn gone_clears_seasons_and_policy_with_zero_runs_still_needs_one() {
        let mut before = present("series:tvdb:1", 0, 0);
        before.seasons = Seasons::from([(1, 10)]);
        let policy = AbsencePolicy {
            complete_runs: 0,
            min_duration: TimeDelta::zero(),
        };
        let d = plan(&[before], &listing(vec![], true), t(0), policy);
        assert_eq!(d[0].transition, Transition::Gone);
        assert!(d[0].entry.seasons.is_empty());
    }

    const RG: &str = "album:mbrg:1b022e01-4da6-387b-8658-8678046e4cef";

    #[test]
    fn album_tracks_follow_the_season_rules() {
        // Two libraries list the album: the larger count wins.
        let d = plan(
            &[],
            &listing(vec![album(RG, 7), album(RG, 11)], true),
            t(0),
            POLICY,
        );
        assert_eq!(d[0].transition, Transition::Appeared);
        assert_eq!(d[0].entry.tracks, Some(11));
        let mut state = vec![d[0].entry.clone()];
        // An incomplete listing only adds evidence.
        let d = plan(&state, &listing(vec![album(RG, 3)], false), t(1), POLICY);
        assert_eq!(d[0].entry.tracks, Some(11));
        // A complete one is authoritative.
        let d = plan(&state, &listing(vec![album(RG, 9)], true), t(1), POLICY);
        assert_eq!(d[0].entry.tracks, Some(9));
        // Gone after two complete runs: no tracks any more; back with the
        // listed count.
        step(&mut state, &listing(vec![], true), t(10));
        step(&mut state, &listing(vec![], true), t(20));
        assert!(state[0].gone_since.is_some());
        assert_eq!(state[0].tracks, None);
        let d = plan(&state, &listing(vec![album(RG, 4)], false), t(21), POLICY);
        assert_eq!(d[0].transition, Transition::Returned);
        assert_eq!(d[0].entry.tracks, Some(4));
    }
}
