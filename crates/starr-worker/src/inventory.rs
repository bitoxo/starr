//! `inventory.sync`: read the libraries of every consume instance
//! (Jellyfin, Emby: movies and series, and the music libraries' albums
//! where the music category watches there; Navidrome: albums), plan
//! present/absent per natural key with `starr_core::inventory::plan`, and
//! store the result. Albums are keyed by their release-group MBID only
//! (spec 0006 Q7 B); an album without one counts as unmatched.
//!
//! Which instances: those at least one category watches in (`consume`
//! role, spec 0003 slice 2). Without any, the scheduler does not even
//! enqueue the job (`SystemJobs::has_work`). For services whose libraries
//! no account scan lists (a consume service without accounts) the sync also
//! stores the library listing the admin maps to categories; Jellyfin, Emby
//! and Navidrome (since spec 0006 slice 7) get it from `accounts.scan`.

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use starr_core::adapters::{AdapterError, Listing};
use starr_core::inventory::{self, AbsencePolicy, Entry, Observation, Transition};
use starr_core::jobs::{Expectation, ItemOutcome, Report, ReportItem};
use starr_core::library::{
    InventoryKey, ItemKind, Library, LibraryContent, MediaItem, MediaLibrary, seasons_map,
};
use starr_core::members::Category;
use starr_db::Database;
use starr_db::inventory::InventoryRow;

use crate::services::{Failure, Openers, instance_client};
use crate::{JobContext, JobError, adapters};

/// What one instance listed in this run.
struct Snapshot {
    server_id: Option<String>,
    libraries: Listing<MediaLibrary>,
    items: Listing<Listed>,
}

/// One item and the library it was listed in (spec 0005 M4: inventory by
/// library, so availability follows the category's libraries).
struct Listed {
    library_id: String,
    item: MediaItem,
}

/// The items of all libraries except books, and music libraries only when
/// `music` (the music category watches in the instance). Complete only if
/// the library list and every library's items are complete.
async fn snapshot<L: Library>(library: &L, music: bool) -> Result<Snapshot, AdapterError> {
    let server_id = library.server_id().await?;
    let libraries = library.libraries().await?;
    let mut complete = libraries.complete;
    let mut items = Vec::new();
    for lib in &libraries.items {
        // Mixed libraries have no collection type (`Other`); the item query
        // only asks for movies and series there.
        let skip = match lib.content {
            LibraryContent::Books => true,
            LibraryContent::Music => !music,
            LibraryContent::Movies | LibraryContent::Series | LibraryContent::Other => false,
        };
        if skip {
            continue;
        }
        let listing = library.items(&lib.id).await?;
        complete &= listing.complete;
        items.extend(listing.items.into_iter().map(|item| Listed {
            library_id: lib.id.clone(),
            item,
        }));
    }
    Ok(Snapshot {
        server_id,
        libraries,
        items: Listing { items, complete },
    })
}

/// Instance IDs the job syncs: every consume instance, or only the one in
/// the payload (if it is a consume instance).
async fn consume_instances(ctx: &JobContext) -> Result<Vec<i64>, JobError> {
    let mut ids = ctx.db.consume_instance_ids().await?;
    if let Some(id) = ctx.payload.get("instanceId") {
        let id = id
            .as_i64()
            .ok_or_else(|| JobError::Failed("instanceId must be an integer".to_owned()))?;
        ids.retain(|known| *known == id);
    }
    Ok(ids)
}

pub(crate) async fn sync(ctx: &JobContext, openers: Openers<'_>) -> Result<Report, JobError> {
    let ids = consume_instances(ctx).await?;
    let mut items = Vec::with_capacity(ids.len());
    let mut counters: HashMap<&'static str, u64> = HashMap::new();
    let (mut ok, mut failed, mut skipped, mut incomplete) = (0u64, 0u64, 0u64, 0u64);
    for id in ids {
        if ctx.cancel.is_cancelled() {
            return Err(JobError::Cancelled);
        }
        let outcome = tokio::select! {
            outcome = sync_one(&ctx.db, openers, id, Utc::now()) => outcome?,
            () = ctx.cancel.cancelled() => return Err(JobError::Cancelled),
        };
        match &outcome {
            Synced::Done {
                counts, complete, ..
            } => {
                ok += 1;
                if !complete {
                    incomplete += 1;
                }
                for (name, value) in counts {
                    *counters.entry(name).or_default() += value;
                }
            }
            Synced::Failed { .. } => failed += 1,
            Synced::Skipped { .. } => skipped += 1,
        }
        items.push(outcome.into_item());
    }

    let total = ok + failed + skipped;
    let expectation = if total == skipped {
        Expectation::NotApplicable
    } else if failed > 0 {
        Expectation::Unmet(format!("{failed} of {} instances failed", ok + failed))
    } else if incomplete > 0 {
        Expectation::Unmet(format!(
            "{incomplete} of {ok} listings incomplete; absence not evaluated for them"
        ))
    } else {
        Expectation::Met(format!("{ok} instances listed completely"))
    };
    let mut report = Report::new(total, ok, failed, skipped, expectation)?
        .with_counter("incomplete", incomplete);
    let mut names: Vec<_> = counters.into_iter().collect();
    names.sort_unstable();
    for (name, value) in names {
        report = report.with_counter(name, value);
    }
    for item in items {
        report = report.with_item(item);
    }
    Ok(report)
}

/// How one instance's sync ended.
enum Synced {
    Done {
        target: String,
        counts: Vec<(&'static str, u64)>,
        complete: bool,
    },
    Failed {
        target: String,
        failure: Failure,
    },
    Skipped {
        target: String,
        reason: &'static str,
    },
}

impl Synced {
    fn into_item(self) -> ReportItem {
        match self {
            Self::Done {
                target,
                counts,
                complete,
            } => {
                let mut detail: Vec<String> =
                    counts.iter().map(|(k, v)| format!("{k} {v}")).collect();
                if !complete {
                    detail.push("listing incomplete".to_owned());
                }
                ReportItem {
                    target,
                    outcome: ItemOutcome::Succeeded,
                    code: None,
                    detail: Some(detail.join(", ")),
                }
            }
            Self::Failed { target, failure } => ReportItem {
                target,
                outcome: ItemOutcome::Failed,
                code: Some(failure.code.to_owned()),
                detail: Some(failure.detail),
            },
            Self::Skipped { target, reason } => ReportItem {
                target,
                outcome: ItemOutcome::Skipped,
                code: None,
                detail: Some(reason.to_owned()),
            },
        }
    }
}

async fn sync_one(
    db: &Database,
    openers: Openers<'_>,
    id: i64,
    now: DateTime<Utc>,
) -> Result<Synced, JobError> {
    let Some(conn) = db.service_connection(id).await? else {
        return Ok(Synced::Skipped {
            target: format!("service {id}"),
            reason: "removed before the sync ran",
        });
    };
    let target = format!("{} ({})", conn.name, conn.adapter_type);
    let client = match instance_client(openers, &conn) {
        Ok((_, client)) => client,
        Err(failure) => return Ok(Synced::Failed { target, failure }),
    };
    let music = db.watches_in(Category::Music, id).await?;
    let result = match conn.adapter_type.as_str() {
        "jellyfin" => snapshot(&starr_adapter_jellyfin::Jellyfin::new(client), music).await,
        "emby" => snapshot(&starr_adapter_emby::Emby::new(client), music).await,
        "navidrome" => snapshot(&starr_adapter_navidrome::Navidrome::new(client), music).await,
        _ => {
            return Ok(Synced::Skipped {
                target,
                reason: "this service type has no libraries to read",
            });
        }
    };
    let snapshot = match result {
        Ok(snapshot) => snapshot,
        Err(err) => {
            tracing::warn!(service = id, code = err.code(), error = %err, "inventory listing failed");
            return Ok(Synced::Failed {
                target,
                failure: err.into(),
            });
        }
    };

    // The admin maps libraries from the last listing; `accounts.scan` keeps
    // it for services with accounts.
    if !adapters::has_accounts(&conn.adapter_type) {
        db.store_library_listing(id, &snapshot.libraries, now)
            .await?;
    }
    let previous = db.inventory(id).await?;
    let (rows, counts) = apply(&previous, snapshot, now, AbsencePolicy::DEFAULT);
    let complete = counts.complete;
    if !db.store_inventory(id, &rows).await? {
        return Ok(Synced::Skipped {
            target,
            reason: "removed while the sync ran",
        });
    }
    tracing::info!(
        service = id,
        complete,
        changed = rows.len(),
        "inventory synced"
    );
    Ok(Synced::Done {
        target,
        counts: counts.into_vec(),
        complete,
    })
}

/// Counters of one instance's run.
#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    complete: bool,
    listed: u64,
    unmatched: u64,
    appeared: u64,
    returned: u64,
    unconfirmed: u64,
    gone: u64,
}

impl Counts {
    fn record(&mut self, transition: Transition) {
        match transition {
            Transition::Appeared => self.appeared += 1,
            Transition::Returned => self.returned += 1,
            Transition::Unconfirmed => self.unconfirmed += 1,
            Transition::Gone => self.gone += 1,
            Transition::StillPresent => {}
        }
    }

    fn into_vec(self) -> Vec<(&'static str, u64)> {
        vec![
            ("listed", self.listed),
            ("unmatched", self.unmatched),
            ("appeared", self.appeared),
            ("returned", self.returned),
            ("unconfirmed", self.unconfirmed),
            ("gone", self.gone),
        ]
    }
}

fn entry(row: &InventoryRow) -> Entry {
    Entry {
        key: InventoryKey::from_stored(row.natural_key.clone()),
        present_since: row.present_since,
        last_seen: row.last_seen,
        missing_runs: row.missing_runs,
        gone_since: row.gone_since,
        seasons: row.seasons.clone(),
        tracks: row.tracks,
    }
}

/// The libraries to store for a row: what this run listed, joined with what
/// was known before when the listing was incomplete (a missing library
/// there proves nothing). `None` when neither knows any.
fn libraries_of(
    listed: Option<&BTreeSet<String>>,
    before: Option<&InventoryRow>,
    complete: bool,
) -> Option<Vec<String>> {
    let mut set: BTreeSet<String> = listed.cloned().unwrap_or_default();
    if !complete || listed.is_none() {
        if let Some(known) = before.and_then(|b| b.library_ids.as_ref()) {
            set.extend(known.iter().cloned());
        } else if listed.is_none() {
            return None;
        }
    }
    Some(set.into_iter().collect())
}

/// The key `item` is kept under, for kinds the inventory table holds:
/// movies, series and albums (by release group only, ADR 0013).
fn stored_key(item: &MediaItem) -> Option<InventoryKey> {
    match item.kind {
        ItemKind::Movie | ItemKind::Series | ItemKind::Album => {
            InventoryKey::of(item.kind, &item.provider_ids)
        }
        ItemKind::Season | ItemKind::Episode => None,
    }
}

/// Plan one run and turn the decisions into rows to store. Items without a
/// usable provider ID are "unmatched". A present row whose remote ID is
/// still listed (its provider IDs vanished, or changed its key, e.g. a TVDB
/// ID added by a metadata refresh) counts as seen, so it never drifts
/// towards "gone" while the item is still there. Rows record the libraries
/// their item was listed in.
fn apply(
    previous: &[InventoryRow],
    snapshot: Snapshot,
    now: DateTime<Utc>,
    policy: AbsencePolicy,
) -> (Vec<InventoryRow>, Counts) {
    let mut counts = Counts {
        complete: snapshot.items.complete,
        listed: snapshot.items.items.len() as u64,
        ..Counts::default()
    };
    let mut observed: HashMap<InventoryKey, MediaItem> = HashMap::new();
    let mut observations = Vec::new();
    // Libraries each remote item and each natural key was listed in: a
    // title can be in two libraries, also as two items (two files).
    let mut listed_remote_ids: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut key_libraries: HashMap<InventoryKey, BTreeSet<String>> = HashMap::new();
    for Listed { library_id, item } in snapshot.items.items {
        listed_remote_ids
            .entry(item.remote_id.clone())
            .or_default()
            .insert(library_id.clone());
        if let Some(key) = stored_key(&item) {
            key_libraries
                .entry(key.clone())
                .or_default()
                .insert(library_id);
            observations.push(Observation {
                key: key.clone(),
                seasons: seasons_map(item.units.seasons()),
                tracks: item.units.tracks(),
            });
            observed.insert(key, item);
        } else {
            counts.unmatched += 1;
        }
    }
    for row in previous {
        let key = InventoryKey::from_stored(row.natural_key.clone());
        if row.gone_since.is_none()
            && listed_remote_ids.contains_key(&row.remote_id)
            && !observed.contains_key(&key)
        {
            observations.push(Observation {
                key,
                seasons: row.seasons.clone(),
                tracks: row.tracks,
            });
        }
    }

    let entries: Vec<Entry> = previous.iter().map(entry).collect();
    let decisions = inventory::plan(
        &entries,
        &Listing {
            items: observations,
            complete: counts.complete,
        },
        now,
        policy,
    );
    let by_key: HashMap<&str, &InventoryRow> = previous
        .iter()
        .map(|r| (r.natural_key.as_str(), r))
        .collect();

    let mut rows = Vec::with_capacity(decisions.len());
    for decision in decisions {
        counts.record(decision.transition);
        let e = decision.entry;
        let before = by_key.get(e.key.as_str()).copied();
        let row = match (observed.get(&e.key), before) {
            (Some(item), _) => InventoryRow {
                library_ids: libraries_of(key_libraries.get(&e.key), before, counts.complete),
                natural_key: e.key.to_string(),
                kind: item.kind.as_str().to_owned(),
                tmdb_id: item.provider_ids.tmdb,
                tvdb_id: item.provider_ids.tvdb,
                imdb_id: item.provider_ids.imdb.clone(),
                remote_id: item.remote_id.clone(),
                server_id: snapshot.server_id.clone(),
                title: item.title.clone(),
                year: item.year,
                seasons: e.seasons,
                tracks: e.tracks,
                present_since: e.present_since,
                last_seen: e.last_seen,
                missing_runs: e.missing_runs,
                gone_since: e.gone_since,
            },
            (None, Some(before)) => InventoryRow {
                // Gone rows are not listed: their last libraries stay.
                library_ids: libraries_of(
                    listed_remote_ids.get(&before.remote_id),
                    Some(before),
                    counts.complete,
                ),
                seasons: e.seasons,
                tracks: e.tracks,
                present_since: e.present_since,
                last_seen: e.last_seen,
                missing_runs: e.missing_runs,
                gone_since: e.gone_since,
                ..before.clone()
            },
            // The planner only decides about listed or known keys.
            (None, None) => continue,
        };
        rows.push(row);
    }
    (rows, counts)
}

#[cfg(test)]
mod tests {
    use chrono::{TimeDelta, TimeZone};
    use starr_core::library::{ProviderIds, SeasonPresence, UnitCounts};

    use super::*;

    fn t(hours: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap() + TimeDelta::hours(hours)
    }

    fn series(remote: &str, tvdb: Option<u32>, seasons: &[(u32, u32)]) -> MediaItem {
        MediaItem {
            remote_id: remote.to_owned(),
            kind: ItemKind::Series,
            provider_ids: ProviderIds {
                tvdb,
                ..ProviderIds::default()
            },
            title: "Show".to_owned(),
            year: Some(2020),
            units: UnitCounts::Seasons(
                seasons
                    .iter()
                    .map(|&(number, episodes)| SeasonPresence { number, episodes })
                    .collect(),
            ),
        }
    }

    fn snap(items: Vec<MediaItem>, complete: bool) -> Snapshot {
        snap_in(items.into_iter().map(|i| ("lib", i)).collect(), complete)
    }

    fn snap_in(items: Vec<(&str, MediaItem)>, complete: bool) -> Snapshot {
        Snapshot {
            server_id: Some("srv".to_owned()),
            libraries: Listing {
                items: Vec::new(),
                complete: true,
            },
            items: Listing {
                items: items
                    .into_iter()
                    .map(|(library, item)| Listed {
                        library_id: library.to_owned(),
                        item,
                    })
                    .collect(),
                complete,
            },
        }
    }

    #[test]
    fn rows_record_their_libraries() {
        let mut state = Vec::new();
        let show = || series("r1", Some(1), &[(1, 3)]);
        let (rows, _) = apply(
            &state,
            snap_in(vec![("tv", show()), ("adult", show())], true),
            t(0),
            AbsencePolicy::DEFAULT,
        );
        assert_eq!(rows[0].library_ids, Some(vec!["adult".into(), "tv".into()]));
        merge(&mut state, rows);
        // Incomplete and only in one: the other is still assumed.
        let (rows, _) = apply(
            &state,
            snap_in(vec![("tv", show())], false),
            t(1),
            AbsencePolicy::DEFAULT,
        );
        assert_eq!(rows[0].library_ids, Some(vec!["adult".into(), "tv".into()]));
        merge(&mut state, rows);
        // Complete: exactly what was listed.
        let (rows, _) = apply(
            &state,
            snap_in(vec![("tv", show())], true),
            t(2),
            AbsencePolicy::DEFAULT,
        );
        assert_eq!(rows[0].library_ids, Some(vec!["tv".into()]));
        merge(&mut state, rows);
        // Gone: the last known libraries stay.
        for hours in [24, 48] {
            let (rows, _) = apply(&state, snap(vec![], true), t(hours), AbsencePolicy::DEFAULT);
            merge(&mut state, rows);
        }
        assert!(state[0].gone_since.is_some());
        assert_eq!(state[0].library_ids, Some(vec!["tv".into()]));
    }

    #[test]
    fn a_title_listed_as_two_items_keeps_both_libraries() {
        let (rows, _) = apply(
            &[],
            snap_in(
                vec![
                    ("movies", series("r1", Some(1), &[(1, 3)])),
                    ("4k", series("r2", Some(1), &[(1, 3)])),
                ],
                true,
            ),
            t(0),
            AbsencePolicy::DEFAULT,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].library_ids,
            Some(vec!["4k".into(), "movies".into()])
        );
    }

    #[test]
    fn rows_from_before_libraries_were_recorded_get_them() {
        let mut state = Vec::new();
        let (rows, _) = apply(
            &state,
            snap(vec![series("r1", Some(1), &[])], true),
            t(0),
            AbsencePolicy::DEFAULT,
        );
        merge(&mut state, rows);
        state[0].library_ids = None;
        let (rows, _) = apply(
            &state,
            snap_in(vec![("tv", series("r1", Some(1), &[]))], false),
            t(1),
            AbsencePolicy::DEFAULT,
        );
        assert_eq!(rows[0].library_ids, Some(vec!["tv".into()]));
    }

    /// Store returned rows over the previous state, like the database does.
    fn merge(state: &mut Vec<InventoryRow>, rows: Vec<InventoryRow>) {
        for row in rows {
            match state.iter_mut().find(|r| r.natural_key == row.natural_key) {
                Some(r) => *r = row,
                None => state.push(row),
            }
        }
    }

    #[test]
    fn albums_are_stored_by_release_group_with_their_tracks_and_untagged_ones_are_unmatched() {
        let album = |remote: &str, rg: Option<u128>, tracks: u32| MediaItem {
            remote_id: remote.to_owned(),
            kind: ItemKind::Album,
            provider_ids: ProviderIds {
                mb_release_group: rg.map(uuid::Uuid::from_u128),
                ..ProviderIds::default()
            },
            title: "OK Computer".to_owned(),
            year: Some(1997),
            units: UnitCounts::Tracks(tracks),
        };
        let rg = 0x1b02_2e01_4da6_387b_8658_8678_046e_4cef;
        let mut state = Vec::new();
        let (rows, counts) = apply(
            &state,
            snap(vec![album("a1", Some(rg), 12), album("a2", None, 9)], true),
            t(0),
            AbsencePolicy::DEFAULT,
        );
        assert_eq!(counts.unmatched, 1, "never matched by name");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].natural_key,
            "album:mbrg:1b022e01-4da6-387b-8658-8678046e4cef"
        );
        assert_eq!(rows[0].kind, "album");
        assert_eq!(rows[0].tracks, Some(12));
        assert!(rows[0].seasons.is_empty());
        merge(&mut state, rows);
        // A smaller count from an incomplete listing proves nothing.
        let (rows, _) = apply(
            &state,
            snap(vec![album("a1", Some(rg), 3)], false),
            t(1),
            AbsencePolicy::DEFAULT,
        );
        assert_eq!(rows[0].tracks, Some(12));
        // Gone after two complete runs without it.
        for hours in [24, 48] {
            let (rows, _) = apply(&state, snap(vec![], true), t(hours), AbsencePolicy::DEFAULT);
            merge(&mut state, rows);
        }
        assert!(state[0].gone_since.is_some());
        assert_eq!(state[0].tracks, None);
    }

    #[test]
    fn rows_carry_metadata_and_seasons() {
        let (rows, counts) = apply(
            &[],
            snap(
                vec![
                    series("r1", Some(81189), &[(1, 7)]),
                    series("r2", None, &[]),
                ],
                true,
            ),
            t(0),
            AbsencePolicy::DEFAULT,
        );
        assert_eq!(counts.appeared, 1);
        assert_eq!(counts.unmatched, 1);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.natural_key, "series:tvdb:81189");
        assert_eq!(row.kind, "series");
        assert_eq!(row.remote_id, "r1");
        assert_eq!(row.server_id.as_deref(), Some("srv"));
        assert_eq!(row.seasons, [(1, 7)].into());
        assert_eq!(row.present_since, Some(t(0)));
    }

    #[test]
    fn items_go_only_after_two_complete_runs_and_keep_their_metadata() {
        let mut state = Vec::new();
        let (rows, _) = apply(
            &state,
            snap(vec![series("r1", Some(1), &[(1, 3)])], true),
            t(0),
            AbsencePolicy::DEFAULT,
        );
        merge(&mut state, rows);
        let (rows, counts) = apply(&state, snap(vec![], false), t(24), AbsencePolicy::DEFAULT);
        assert!(rows.is_empty() && counts.gone == 0, "incomplete: nothing");
        let (rows, counts) = apply(&state, snap(vec![], true), t(24), AbsencePolicy::DEFAULT);
        assert_eq!(counts.unconfirmed, 1);
        merge(&mut state, rows);
        let (rows, counts) = apply(&state, snap(vec![], true), t(25), AbsencePolicy::DEFAULT);
        assert_eq!(counts.gone, 1);
        assert_eq!(rows[0].remote_id, "r1", "metadata kept");
        assert_eq!(rows[0].gone_since, Some(t(25)));
        assert!(rows[0].seasons.is_empty());
    }

    #[test]
    fn an_item_that_lost_its_provider_ids_stays_present() {
        let mut state = Vec::new();
        let (rows, _) = apply(
            &state,
            snap(vec![series("r1", Some(1), &[(1, 3)])], true),
            t(0),
            AbsencePolicy::DEFAULT,
        );
        merge(&mut state, rows);
        for hours in [24, 48, 72] {
            let (rows, counts) = apply(
                &state,
                snap(vec![series("r1", None, &[])], true),
                t(hours),
                AbsencePolicy::DEFAULT,
            );
            assert_eq!((counts.unconfirmed, counts.gone), (0, 0), "at {hours}h");
            merge(&mut state, rows);
        }
        assert_eq!(state[0].missing_runs, 0);
        assert_eq!(state[0].seasons, [(1, 3)].into());
        assert_eq!(state[0].last_seen, t(72));
    }

    #[test]
    fn an_item_whose_key_changed_does_not_go_under_its_old_key() {
        let mut state = Vec::new();
        let mut show = series("r1", None, &[(1, 3)]);
        show.provider_ids.tmdb = Some(1396);
        let (rows, _) = apply(&state, snap(vec![show], true), t(0), AbsencePolicy::DEFAULT);
        merge(&mut state, rows);
        assert_eq!(state[0].natural_key, "series:tmdb:1396");
        // A metadata refresh adds the TVDB ID: the key becomes tvdb.
        for hours in [24, 48] {
            let mut show = series("r1", Some(81189), &[(1, 3)]);
            show.provider_ids.tmdb = Some(1396);
            let (rows, counts) = apply(
                &state,
                snap(vec![show], true),
                t(hours),
                AbsencePolicy::DEFAULT,
            );
            assert_eq!(counts.gone + counts.unconfirmed, 0, "at {hours}h");
            merge(&mut state, rows);
        }
        assert!(state.iter().all(|r| r.gone_since.is_none()));
    }
}
