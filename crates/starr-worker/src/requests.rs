//! `requests.submit` and `acquisitions.sync` (spec 0003 slice 4).
//!
//! Submission hands pending acquisitions to their source, one title at a
//! time (the worker runs one job at a time, and the adapters serialize
//! writes per natural key on top): a title that is not in the source is
//! added with the acquisition's quality profile, root folder and minimum
//! availability; a title that is there only gets more monitoring (D14:
//! Starr never unmonitors, deletes or changes anything else). Every write
//! is read back by the adapter; a season that the read-back does not show
//! as monitored fails with that reason (lesson 9). Each title is its own
//! unit: one failing title never stops the others (lesson 15).
//!
//! Sync reads queue and file state per title and derives each unit's
//! state with `starr_core::requests::observe`, using the inventory of the
//! category's consume services where there are any (D15). A unit the
//! source has but the consume services' inventory does not ("arriving")
//! asks for an early `inventory.sync` instead of waiting for the hourly one
//! (maintainer's acceptance test, 2026-11-02), at most every
//! [`LIBRARY_REFRESH_INTERVAL`] and only during the first
//! [`LIBRARY_REFRESH_WINDOW`] of arriving: Starr does not make the media
//! server scan, so a title it never lists (library not covering the folder,
//! no provider IDs, scans every 12 hours) falls back to the hourly schedule
//! instead of a full library walk every few minutes forever.
//!
//! Removing requests (spec 0004): submission remembers what Starr changed
//! in the source (added the title, switched a unit's monitoring on), and
//! sends the stops queued when nobody wants a title any more: the source
//! unmonitors exactly what Starr turned on, read back, held like every
//! other write while writes are paused. Sync also looks at available
//! titles the consume services list only as gone: without a file in the
//! source they end as removed and can be requested again.

use std::collections::{BTreeSet, HashSet};

use chrono::{DateTime, TimeDelta, Utc};
use starr_core::adapters::AdapterError;
use starr_core::categories::is_private;
use starr_core::jobs::{Expectation, ItemOutcome, JobKind, Report, ReportItem, Trigger};
use starr_core::members::Category;
use starr_core::requests::{self as rules, AcquisitionState, Observation, Observed};
use starr_core::source::{
    AddOutcome, AddRequest, Deviation, FileState, Files, LibraryItem, Lookup, MonitorScope,
    NaturalKey, QueueEntry, UnitId, UnitProgress, WriteReport,
};
use starr_db::catalog::ItemKind;
use starr_db::jobs::NewJob;
use starr_db::requests::{
    Actor, EventTrigger, SceneFile, SourceChange, StopUnit, SubmitUnit, SyncUnit,
};
use uuid::Uuid;

use crate::adapters::AnySource;
use crate::services::{CODE_ADAPTER_UNKNOWN, Openers, instance_client};
use crate::{JobContext, JobError};

const SUBMIT: Actor<'static> = Actor {
    trigger: EventTrigger::System,
    name: JobKind::RequestsSubmit.name(),
};
const SYNC: Actor<'static> = Actor {
    trigger: EventTrigger::System,
    name: JobKind::AcquisitionsSync.name(),
};

/// Least time from one `inventory.sync` run to the next that
/// `acquisitions.sync` asks for while units are arriving.
const LIBRARY_REFRESH_INTERVAL: TimeDelta = TimeDelta::minutes(5);
/// How long after a unit started arriving `acquisitions.sync` keeps asking
/// for early `inventory.sync` runs; afterwards the hourly schedule decides.
const LIBRARY_REFRESH_WINDOW: TimeDelta = TimeDelta::hours(1);

/// Most rounds of "load due acquisitions, submit them" per run.
const MAX_ROUNDS: usize = 10;

/// Error code when a title lacks the natural key its source needs.
const CODE_NO_NATURAL_KEY: &str = "no_natural_key";
/// Error code when the read-back does not show what was written.
const CODE_NOT_MONITORED: &str = "not_monitored";
/// Error code when the read-back still shows a stopped unit as monitored.
const CODE_STILL_MONITORED: &str = "still_monitored";
/// Delay before a stop that failed in passing is tried again.
const STOP_RETRY: TimeDelta = TimeDelta::minutes(15);

/// Consecutive runs of `units` with the same key, in order.
fn groups<T, K: PartialEq>(units: Vec<T>, key: impl Fn(&T) -> K) -> Vec<Vec<T>> {
    let mut out: Vec<Vec<T>> = Vec::new();
    for unit in units {
        match out.last_mut() {
            Some(group) if group.first().is_some_and(|g| key(g) == key(&unit)) => group.push(unit),
            _ => out.push(vec![unit]),
        }
    }
    out
}

/// How the source knows one unit: the natural key of its title and, for
/// a season or an album, the unit within it.
#[derive(Debug, Clone, Copy)]
struct UnitKeys {
    season: Option<u32>,
    release_group: Option<Uuid>,
    tmdb: Option<u64>,
    tvdb: Option<u64>,
    mb_artist: Option<Uuid>,
    /// Scenes: the `StashDB` UUID (Whisparr's `foreignId`).
    stash_scene: Option<Uuid>,
}

impl UnitKeys {
    /// The unit within its title: a season or an album; `None` for a movie.
    fn unit(self) -> Option<UnitId> {
        match (self.release_group, self.season) {
            (Some(release_group), _) => Some(UnitId::ReleaseGroup(release_group)),
            (None, Some(n)) => Some(UnitId::Season(n)),
            (None, None) => None,
        }
    }

    /// The natural key the source keeps the title by: TMDB for movies, TVDB
    /// for series, the artist's MBID for albums (Lidarr), the `StashDB` UUID
    /// for scenes (Whisparr).
    fn natural_key(self) -> Result<NaturalKey, String> {
        let id = |v: Option<u64>| v.and_then(|v| u32::try_from(v).ok());
        if let Some(scene) = self.stash_scene {
            return Ok(NaturalKey::StashScene(scene));
        }
        match self.unit() {
            None => id(self.tmdb)
                .map(NaturalKey::TmdbMovie)
                .ok_or_else(|| "the movie has no TMDB ID".to_owned()),
            Some(UnitId::Season(_)) => id(self.tvdb)
                .map(NaturalKey::TvdbSeries)
                .ok_or_else(|| "the series has no TVDB ID, which Sonarr needs".to_owned()),
            Some(UnitId::ReleaseGroup(_)) => self
                .mb_artist
                .map(NaturalKey::MbArtist)
                .ok_or_else(|| "the artist has no MusicBrainz ID, which Lidarr needs".to_owned()),
        }
    }

    /// The catalog kind of the title, for the consume services' inventory.
    const fn title_kind(self) -> ItemKind {
        match (self.release_group, self.season, self.stash_scene) {
            (Some(_), _, _) => ItemKind::Artist,
            (None, Some(_), _) => ItemKind::Series,
            (None, None, Some(_)) => ItemKind::Scene,
            (None, None, None) => ItemKind::Movie,
        }
    }
}

/// How logs and run reports name something of a private category.
const PRIVATE: &str = "[adult]";
/// The report detail of a private unit in place of the source's words.
const PRIVATE_DETAIL: &str = "details withheld (private category)";

/// A title or a source's message as logs may show it (spec 0007, server
/// logs): the text itself, or `[adult]` for the private category, at
/// every level.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LogTitle<'a> {
    private: bool,
    text: &'a str,
}

impl<'a> LogTitle<'a> {
    pub(crate) fn new(category: Category, text: &'a str) -> Self {
        Self {
            private: is_private(category),
            text,
        }
    }
}

impl std::fmt::Display for LogTitle<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.private {
            f.write_str(PRIVATE)
        } else {
            f.write_str(self.text)
        }
    }
}

/// A unit `requests.submit` or `acquisitions.sync` works on.
trait SourceUnit {
    fn keys(&self) -> UnitKeys;
    /// The title (the movie, the series, the artist).
    fn title(&self) -> &str;
    /// The unit's own catalog title (the album's, for music).
    fn unit_title(&self) -> &str;
    fn category(&self) -> Category;
    fn acquisition_id(&self) -> i64;

    /// The title as logs may name it ([`LogTitle`]).
    fn log_title(&self) -> LogTitle<'_> {
        LogTitle::new(self.category(), self.title())
    }

    /// A report detail that may carry the source's own words (which can
    /// name the title): withheld for a private unit, which keeps its code.
    fn report_detail(&self, detail: &str) -> String {
        if is_private(self.category()) {
            PRIVATE_DETAIL.to_owned()
        } else {
            detail.to_owned()
        }
    }

    /// How reports name the unit: "Title", "Title S02", "Artist – Album";
    /// a private unit only by its acquisition id (spec 0007: run reports
    /// show ids, never titles).
    fn target(&self) -> String {
        if is_private(self.category()) {
            return format!("{PRIVATE} acquisition #{}", self.acquisition_id());
        }
        match self.keys().unit() {
            None => self.title().to_owned(),
            Some(UnitId::Season(n)) => format!("{} S{n:02}", self.title()),
            Some(UnitId::ReleaseGroup(_)) => format!("{} – {}", self.title(), self.unit_title()),
        }
    }

    /// How a read-back reason names the unit.
    fn described(&self) -> String {
        match self.keys().unit() {
            None if self.keys().stash_scene.is_some() => "the scene".to_owned(),
            None => "the movie".to_owned(),
            Some(UnitId::Season(n)) => format!("season {n}"),
            Some(UnitId::ReleaseGroup(_)) => format!("the album {}", self.unit_title()),
        }
    }
}

macro_rules! source_unit {
    ($($ty:ty),+) => {$(
        impl SourceUnit for $ty {
            fn keys(&self) -> UnitKeys {
                UnitKeys {
                    season: self.season_number,
                    release_group: self.release_group,
                    tmdb: self.tmdb_id,
                    tvdb: self.tvdb_id,
                    mb_artist: self.mb_artist,
                    stash_scene: self.stash_scene,
                }
            }

            fn title(&self) -> &str {
                &self.title
            }

            fn unit_title(&self) -> &str {
                &self.unit_title
            }

            fn category(&self) -> Category {
                self.category
            }

            fn acquisition_id(&self) -> i64 {
                self.acquisition_id
            }
        }
    )+};
}

source_unit!(SubmitUnit, StopUnit, SyncUnit);

/// What the source monitors for `units` of one title: the title itself (a
/// movie) or exactly these units (seasons, albums).
fn scope_of<'a, U: SourceUnit + 'a>(units: impl IntoIterator<Item = &'a U>) -> MonitorScope {
    let units: BTreeSet<UnitId> = units.into_iter().filter_map(|u| u.keys().unit()).collect();
    if units.is_empty() {
        MonitorScope::Item
    } else {
        MonitorScope::Units(units)
    }
}

fn describe(deviation: &Deviation) -> String {
    match deviation {
        Deviation::ItemNotMonitored => "the title is not monitored".to_owned(),
        Deviation::UnitNotMonitored(unit) => format!("{unit} is not monitored"),
        Deviation::UnitUnexpectedlyMonitored(unit) => format!("{unit} is monitored as well"),
        Deviation::ItemStillMonitored => "the title is still monitored".to_owned(),
        Deviation::QualityProfile { expected, found } => {
            format!("quality profile {found} instead of {expected}")
        }
        Deviation::RootFolder { expected, found } => format!(
            "root folder {} instead of {expected}",
            found.as_deref().unwrap_or("(none)")
        ),
    }
}

/// Tally of one run, turned into a report at the end.
#[derive(Default)]
pub(crate) struct Tally {
    /// Units handed to the run; succeeded + failed + skipped must add up
    /// to it (lesson 15).
    input: u64,
    ok: u64,
    failed: u64,
    skipped: u64,
    retrying: u64,
    /// Of `ok`: stops that went out (or had nothing left to stop).
    stopped: u64,
    items: Vec<ReportItem>,
}

impl Tally {
    fn push(&mut self, target: String, outcome: ItemOutcome, code: Option<&str>, detail: String) {
        match outcome {
            ItemOutcome::Succeeded => self.ok += 1,
            ItemOutcome::Failed => self.failed += 1,
            ItemOutcome::Skipped => self.skipped += 1,
        }
        self.items.push(ReportItem {
            target,
            outcome,
            code: code.map(str::to_owned),
            detail: Some(detail),
        });
    }
}

/// Run one title's work; a database error fails only that title's units
/// still unreported, not the whole run (lesson 15). `targets` are the
/// title's units in the order the work reports them.
async fn per_title<F>(tally: &mut Tally, targets: Vec<String>, work: F) -> Result<(), JobError>
where
    F: AsyncFnOnce(&mut Tally) -> Result<(), JobError>,
{
    tally.input += targets.len() as u64;
    let before = tally.items.len();
    match work(tally).await {
        Ok(()) => Ok(()),
        Err(JobError::Db(err)) => {
            tracing::error!(error = %err, "database error while handling a title");
            let reported = tally.items.len() - before;
            for target in targets.into_iter().skip(reported) {
                tally.push(
                    target,
                    ItemOutcome::Failed,
                    Some("database"),
                    err.to_string(),
                );
            }
            Ok(())
        }
        Err(other) => Err(other),
    }
}

/// The units of `group` that the source instance no longer exists for.
fn instance_gone(tally: &mut Tally, targets: impl Iterator<Item = String>) {
    for target in targets {
        tally.push(
            target,
            ItemOutcome::Skipped,
            None,
            "the source instance was removed while the job ran".to_owned(),
        );
    }
}

/// Write the title of `group` to the source as `lookup` found it: add it,
/// or extend the monitoring of what is there. Returns the read-back and the
/// item as it was before (`None`: Starr added it).
async fn write_title(
    source: &AnySource,
    key: NaturalKey,
    lookup: Lookup,
    scope: &MonitorScope,
    first: &SubmitUnit,
) -> Result<(WriteReport, Option<LibraryItem>), AdapterError> {
    match lookup {
        Lookup::NotInLibrary => {
            let request = AddRequest {
                key,
                quality_profile_id: first.quality_profile_id,
                root_folder_path: first.root_folder_path.clone(),
                scope: scope.clone(),
                search: true,
                minimum_availability: first.minimum_availability,
                metadata_profile_id: first.metadata_profile_id,
            };
            match source.add(&request).await? {
                AddOutcome::Added(report) => Ok((report, None)),
                // Added meanwhile (or by an earlier attempt of ours): only
                // extend what is monitored. Whether that earlier add was
                // ours is unknown, so it counts as not (D14).
                AddOutcome::AlreadyExists(before) => Ok((
                    source.extend_monitoring(key, scope, true).await?,
                    Some(before),
                )),
            }
        }
        Lookup::InLibrary(before) => Ok((
            source.extend_monitoring(key, scope, true).await?,
            Some(before),
        )),
    }
}

/// What Starr changed for one unit: added the title (`before` is `None`)
/// or switched on monitoring that was off (the movie's, or the season's or
/// album's).
fn source_change(before: Option<&LibraryItem>, unit: Option<UnitId>) -> SourceChange {
    match before {
        None => SourceChange {
            added: true,
            monitored: true,
        },
        Some(item) => SourceChange {
            added: false,
            monitored: match unit {
                None => !item.monitored,
                Some(unit) => !item.unit_monitored(unit),
            },
        },
    }
}

/// Report `targets` as held: the private category is switched off (spec
/// 0007, "What switching off does").
fn hold_private(tally: &mut Tally, targets: impl Iterator<Item = String>) {
    for target in targets {
        tally.push(
            target,
            ItemOutcome::Skipped,
            None,
            "held: the private category is switched off".to_owned(),
        );
    }
}

/// Report the units of `group` as held: writes to `source_name` are paused.
fn hold(tally: &mut Tally, group: &[SubmitUnit], source_name: &str) {
    for unit in group {
        tally.push(
            unit.target(),
            ItemOutcome::Skipped,
            None,
            format!("held: writes to {source_name} are paused"),
        );
    }
}

/// Whether writes to instance `id` are paused (or it is gone) now, or,
/// for the private `category`, the category is switched off now.
async fn writes_stopped(ctx: &JobContext, id: i64, category: Category) -> Result<bool, JobError> {
    if is_private(category)
        && !ctx
            .db
            .category_state(category)
            .await?
            .enabled_with_statement()
    {
        return Ok(true);
    }
    Ok(ctx
        .db
        .service_connection(id)
        .await?
        .is_none_or(|c| c.writes_paused))
}

/// Submit one title (all due units of one title at one instance). Writes
/// paused after the units were loaded hold them: the pause is checked again
/// right before the first write, so at most a write already on its way to
/// the source completes.
pub(crate) async fn submit_title(
    ctx: &JobContext,
    openers: Openers<'_>,
    group: &[SubmitUnit],
    tally: &mut Tally,
) -> Result<(), JobError> {
    let Some(first) = group.first() else {
        return Ok(());
    };
    let now = Utc::now();
    // Taxonomy code, reason, and the adapter error deciding on a retry
    // (`None`: a Starr-side problem an admin has to fix).
    let (code, reason, error): (&str, String, Option<AdapterError>) = 'write: {
        let Some(conn) = ctx.db.service_connection(first.service_instance_id).await? else {
            instance_gone(tally, group.iter().map(SourceUnit::target));
            return Ok(());
        };
        if conn.writes_paused {
            hold(tally, group, &conn.name);
            return Ok(());
        }
        let client = match instance_client(openers, &conn) {
            Ok((_, client)) => client,
            Err(failure) => break 'write (failure.code, failure.detail, None),
        };
        let Some(source) = AnySource::new(&conn.adapter_type, client) else {
            break 'write (
                CODE_ADAPTER_UNKNOWN,
                format!("{} is not a source", conn.adapter_type),
                None,
            );
        };
        let key = match first.keys().natural_key() {
            Ok(key) => key,
            Err(reason) => break 'write (CODE_NO_NATURAL_KEY, reason, None),
        };
        let scope = scope_of(group);
        let lookup = tokio::select! {
            result = source.lookup(key) => result,
            () = ctx.cancel.cancelled() => return Err(JobError::Cancelled),
        };
        let lookup = match lookup {
            Ok(lookup) => lookup,
            Err(err) => break 'write (err.code(), err.to_string(), Some(err)),
        };
        // Paused while the lookup ran: nothing is written.
        if writes_stopped(ctx, conn.id, first.category).await? {
            hold(tally, group, &conn.name);
            return Ok(());
        }
        if all_cancelled(ctx, group, tally).await? {
            return Ok(());
        }
        match scene_with_file(ctx, &source, key, &lookup).await? {
            Ok(Some(remote_id)) => {
                return record_present(ctx, &conn.name, group, remote_id, tally).await;
            }
            Ok(None) => {}
            Err(err) => break 'write (err.code(), err.to_string(), Some(err)),
        }
        let result = tokio::select! {
            result = write_title(&source, key, lookup, &scope, first) => result,
            () = ctx.cancel.cancelled() => return Err(JobError::Cancelled),
        };
        match result {
            Ok((report, before)) => {
                record_written(ctx, &conn.name, group, &report, before.as_ref(), tally).await?;
                return Ok(());
            }
            Err(err) => (err.code(), err.to_string(), Some(err)),
        }
    };
    tracing::warn!(
        title = %first.log_title(),
        code,
        reason = %LogTitle::new(first.category, &reason),
        "submission failed"
    );
    for unit in group {
        let retry_at = error
            .as_ref()
            .and_then(|err| rules::resubmit_after(err, unit.attempts + 1))
            .map(|delay| now + delay);
        ctx.db
            .record_submit_failure(unit.acquisition_id, code, &reason, retry_at, SUBMIT, now)
            .await?;
        if retry_at.is_some() {
            tally.retrying += 1;
        }
        tally.push(
            unit.target(),
            ItemOutcome::Failed,
            Some(code),
            unit.report_detail(&reason),
        );
    }
    Ok(())
}

/// Whether every unit of `group` was cancelled meanwhile (spec 0004): then
/// nothing is written and they are reported as skipped. If only some were,
/// the write goes out and their late record queues a stop.
async fn all_cancelled(
    ctx: &JobContext,
    group: &[SubmitUnit],
    tally: &mut Tally,
) -> Result<bool, JobError> {
    for unit in group {
        if ctx.db.submission_pending(unit.acquisition_id).await? {
            return Ok(false);
        }
    }
    for unit in group {
        tally.push(
            unit.target(),
            ItemOutcome::Skipped,
            None,
            "cancelled meanwhile: nothing sent".to_owned(),
        );
    }
    Ok(true)
}

/// A scene Whisparr has with its file is there already (spec 0007, C5):
/// its id there, so nothing is written, not even a search. `None` for
/// anything else (not a scene, not in Whisparr, no file).
async fn scene_with_file(
    ctx: &JobContext,
    source: &AnySource,
    key: NaturalKey,
    lookup: &Lookup,
) -> Result<Result<Option<i64>, AdapterError>, JobError> {
    let (NaturalKey::StashScene(_), Lookup::InLibrary(item)) = (key, lookup) else {
        return Ok(Ok(None));
    };
    let progress = tokio::select! {
        result = source.progress(key) => result,
        () = ctx.cancel.cancelled() => return Err(JobError::Cancelled),
    };
    Ok(progress.map(|p| (p.files.state() == FileState::Complete).then_some(item.remote_id)))
}

/// Record units the source has with their files already (a scene in
/// Whisparr): submitted without any change Starr could stop later, and
/// available at once.
async fn record_present(
    ctx: &JobContext,
    source_name: &str,
    group: &[SubmitUnit],
    remote_id: i64,
    tally: &mut Tally,
) -> Result<(), JobError> {
    let now = Utc::now();
    let remote_id = remote_id.to_string();
    let available = Observed {
        state: AcquisitionState::Available,
        progress: Some(1.0),
        message: None,
        stage: None,
    };
    for unit in group {
        ctx.db
            .confirm_external_ref(
                unit.title_item_id,
                unit.service_instance_id,
                &remote_id,
                now,
            )
            .await?;
        let submitted = ctx
            .db
            .record_submitted(
                unit.acquisition_id,
                Some(&remote_id),
                SourceChange::default(),
                SUBMIT,
                now,
            )
            .await?;
        if !submitted {
            tally.push(
                unit.target(),
                ItemOutcome::Skipped,
                None,
                "cancelled meanwhile: nothing sent".to_owned(),
            );
            continue;
        }
        ctx.db
            .record_observed(unit.acquisition_id, &available, SUBMIT, now)
            .await?;
        tally.push(
            unit.target(),
            ItemOutcome::Succeeded,
            None,
            format!("already in {source_name} with its file: nothing sent"),
        );
    }
    Ok(())
}

/// Record a successful write: units the read-back shows as monitored are
/// submitted (with what Starr changed, `before` being the item before the
/// write), the others fail with the deviation.
async fn record_written(
    ctx: &JobContext,
    source_name: &str,
    group: &[SubmitUnit],
    report: &WriteReport,
    before: Option<&LibraryItem>,
    tally: &mut Tally,
) -> Result<(), JobError> {
    let now = Utc::now();
    let remote_id = report.item.remote_id.to_string();
    if let Some(first) = group.first() {
        ctx.db
            .confirm_external_ref(
                first.title_item_id,
                first.service_instance_id,
                &remote_id,
                now,
            )
            .await?;
    }
    let unmonitored: BTreeSet<UnitId> = report
        .deviations
        .iter()
        .filter_map(|d| match d {
            Deviation::UnitNotMonitored(unit) => Some(*unit),
            _ => None,
        })
        .collect();
    let item_unmonitored = report.deviations.contains(&Deviation::ItemNotMonitored);
    // Profile, folder or extra seasons differing is reported, not failed:
    // the requested units are monitored.
    let notes: Vec<String> = report
        .deviations
        .iter()
        .filter(|d| {
            !matches!(
                d,
                Deviation::UnitNotMonitored(_) | Deviation::ItemNotMonitored
            )
        })
        .map(describe)
        .collect();
    for unit in group {
        let target = unit.target();
        let missing =
            item_unmonitored || unit.keys().unit().is_some_and(|u| unmonitored.contains(&u));
        if missing {
            let reason = format!(
                "{source_name} does not show {} as monitored after the write",
                unit.described()
            );
            ctx.db
                .record_submit_failure(
                    unit.acquisition_id,
                    CODE_NOT_MONITORED,
                    &reason,
                    None,
                    SUBMIT,
                    now,
                )
                .await?;
            tally.push(
                target,
                ItemOutcome::Failed,
                Some(CODE_NOT_MONITORED),
                reason,
            );
            continue;
        }
        ctx.db
            .record_submitted(
                unit.acquisition_id,
                Some(&remote_id),
                source_change(before, unit.keys().unit()),
                SUBMIT,
                now,
            )
            .await?;
        let mut detail = if report.written {
            format!("submitted to {source_name}")
        } else {
            format!("already monitored in {source_name}")
        };
        if !notes.is_empty() {
            detail = format!("{detail} (read-back: {})", notes.join(", "));
        }
        tally.push(target, ItemOutcome::Succeeded, None, detail);
    }
    Ok(())
}

/// What a failed stop leaves behind: retried after [`STOP_RETRY`] when the
/// trouble may pass, else given up (the title stays monitored; the reason
/// is readable for admins).
async fn stop_failed(
    ctx: &JobContext,
    group: &[&StopUnit],
    code: &str,
    reason: &str,
    error: Option<&AdapterError>,
    tally: &mut Tally,
) -> Result<(), JobError> {
    let now = Utc::now();
    let retry_at = error
        .filter(|e| rules::resubmit_after(e, 1).is_some())
        .map(|_| now + STOP_RETRY);
    if let Some(first) = group.first() {
        tracing::warn!(
            title = %first.log_title(),
            code,
            reason = %LogTitle::new(first.category, reason),
            "stopping monitoring failed"
        );
    }
    for unit in group {
        ctx.db
            .record_stop_failure(unit.acquisition_id, code, reason, retry_at, now)
            .await?;
        if retry_at.is_some() {
            tally.retrying += 1;
        }
        tally.push(
            unit.target(),
            ItemOutcome::Failed,
            Some(code),
            unit.report_detail(&format!("stopping monitoring failed: {reason}")),
        );
    }
    Ok(())
}

/// Report the stops of `group` as held: writes to `source_name` are paused.
fn hold_stops(tally: &mut Tally, group: &[&StopUnit], source_name: &str) {
    for unit in group {
        tally.push(
            unit.target(),
            ItemOutcome::Skipped,
            None,
            format!("stop held: writes to {source_name} are paused"),
        );
    }
}

/// Send the stops of one title (all due stop units of one title at one
/// instance, spec 0004 R1): the source unmonitors exactly these units, the
/// movie, these seasons or these albums (never the artist, spec 0006 Q13
/// A), and reads the result back. Paused writes hold
/// them like every other write; a stop an approval took back meanwhile is
/// dropped right before the write.
pub(crate) async fn stop_title(
    ctx: &JobContext,
    openers: Openers<'_>,
    group: &[&StopUnit],
    tally: &mut Tally,
) -> Result<(), JobError> {
    let Some(first) = group.first() else {
        return Ok(());
    };
    let Some(conn) = ctx.db.service_connection(first.service_instance_id).await? else {
        instance_gone(tally, group.iter().map(|u| u.target()));
        return Ok(());
    };
    if conn.writes_paused {
        hold_stops(tally, group, &conn.name);
        return Ok(());
    }
    let client = match instance_client(openers, &conn) {
        Ok((_, client)) => client,
        Err(failure) => {
            return stop_failed(ctx, group, failure.code, &failure.detail, None, tally).await;
        }
    };
    let Some(source) = AnySource::new(&conn.adapter_type, client) else {
        let reason = format!("{} is not a source", conn.adapter_type);
        return stop_failed(ctx, group, CODE_ADAPTER_UNKNOWN, &reason, None, tally).await;
    };
    let key = match first.keys().natural_key() {
        Ok(key) => key,
        Err(reason) => {
            return stop_failed(ctx, group, CODE_NO_NATURAL_KEY, &reason, None, tally).await;
        }
    };
    // Wanted again (an approval took the stop back) or paused meanwhile:
    // nothing is written for those.
    let mut units = Vec::with_capacity(group.len());
    for unit in group {
        if ctx.db.stop_pending(unit.acquisition_id, Utc::now()).await? {
            units.push(*unit);
        } else {
            tally.push(
                unit.target(),
                ItemOutcome::Skipped,
                None,
                "no longer due (wanted again, or its files arrived): nothing to stop".to_owned(),
            );
        }
    }
    if units.is_empty() {
        return Ok(());
    }
    if writes_stopped(ctx, conn.id, first.category).await? {
        hold_stops(tally, &units, &conn.name);
        return Ok(());
    }
    let scope = scope_of(units.iter().copied());
    let result = tokio::select! {
        result = source.stop_monitoring(key, &scope) => result,
        () = ctx.cancel.cancelled() => return Err(JobError::Cancelled),
    };
    let report = match result {
        Ok(report) => Some(report),
        // Not in the source any more: nothing left to stop.
        Err(AdapterError::NotFound(_)) => None,
        Err(err) => {
            return stop_failed(ctx, &units, err.code(), &err.to_string(), Some(&err), tally).await;
        }
    };
    record_stops(ctx, &conn.name, &units, report.as_ref(), tally).await
}

/// Record a stop's read-back (`None`: the title is no longer in the
/// source): units still shown as monitored fail, the others are stopped.
async fn record_stops(
    ctx: &JobContext,
    source_name: &str,
    units: &[&StopUnit],
    report: Option<&WriteReport>,
    tally: &mut Tally,
) -> Result<(), JobError> {
    let now = Utc::now();
    for unit in units {
        let target = unit.target();
        let still_on = report.is_some_and(|r| {
            r.deviations.iter().any(|d| match (d, unit.keys().unit()) {
                (Deviation::ItemStillMonitored, None) => true,
                (Deviation::UnitUnexpectedlyMonitored(on), Some(unit)) => *on == unit,
                _ => false,
            })
        });
        if still_on {
            let reason = format!(
                "{source_name} still shows {} as monitored after the write",
                unit.described()
            );
            stop_failed(
                ctx,
                std::slice::from_ref(unit),
                CODE_STILL_MONITORED,
                &reason,
                None,
                tally,
            )
            .await?;
            continue;
        }
        let restarted = ctx.db.record_stopped(unit.acquisition_id, now).await?;
        tally.stopped += 1;
        let detail = match (report, restarted) {
            (_, true) => format!("stopped monitoring in {source_name}; wanted again, resubmitting"),
            (Some(r), false) if r.written => format!("stopped monitoring in {source_name}"),
            (Some(_), false) => format!("nothing to stop in {source_name}"),
            (None, false) => format!("no longer in {source_name}: nothing to stop"),
        };
        tally.push(target, ItemOutcome::Succeeded, None, detail);
    }
    Ok(())
}

pub(crate) async fn submit(ctx: &JobContext, openers: Openers<'_>) -> Result<Report, JobError> {
    let mut tally = Tally::default();
    // Approvals and retries arriving while this runs are picked up by the
    // next round (their enqueue found this job running). An acquisition
    // counts as new again when it was restarted (its `updated_at` moved);
    // the round limit guards against one that never leaves "pending".
    let mut seen: HashSet<(i64, DateTime<Utc>)> = HashSet::new();
    let mut stops_seen: HashSet<i64> = HashSet::new();
    // While the private category is off (or its operator statement is no
    // longer valid), its writes are held like paused ones (spec 0007, "What
    // switching off does").
    let private_open = ctx
        .db
        .category_state(Category::Adult)
        .await?
        .enabled_with_statement();
    let held_private = |category: Category| is_private(category) && !private_open;
    for _ in 0..MAX_ROUNDS {
        // Stops first: an acquisition nobody wants goes before new ones.
        let mut stops: Vec<StopUnit> = ctx
            .db
            .due_stops(Utc::now())
            .await?
            .into_iter()
            .filter(|u| stops_seen.insert(u.acquisition_id))
            .collect();
        stops.sort_by_key(|u| (u.service_instance_id, u.title_item_id));
        let any_stop = !stops.is_empty();
        for group in groups(stops, |u| (u.service_instance_id, u.title_item_id)) {
            if ctx.cancel.is_cancelled() {
                return Err(JobError::Cancelled);
            }
            if group.first().is_some_and(|u| held_private(u.category)) {
                tally.input += group.len() as u64;
                hold_private(&mut tally, group.iter().map(SourceUnit::target));
                continue;
            }
            let targets = group.iter().map(SourceUnit::target).collect();
            per_title(&mut tally, targets, async |tally: &mut Tally| {
                stop_title(ctx, openers, &group.iter().collect::<Vec<_>>(), tally).await
            })
            .await?;
        }
        let due: Vec<SubmitUnit> = ctx
            .db
            .due_submissions(Utc::now())
            .await?
            .into_iter()
            .filter(|u| seen.insert((u.acquisition_id, u.updated_at)))
            .collect();
        if due.is_empty() && !any_stop {
            break;
        }
        let mut sorted = due;
        sorted.sort_by_key(|u| (u.service_instance_id, u.title_item_id));
        for group in groups(sorted, |u| (u.service_instance_id, u.title_item_id)) {
            if ctx.cancel.is_cancelled() {
                return Err(JobError::Cancelled);
            }
            if group.first().is_some_and(|u| held_private(u.category)) {
                tally.input += group.len() as u64;
                hold_private(&mut tally, group.iter().map(SourceUnit::target));
                continue;
            }
            let targets = group.iter().map(SourceUnit::target).collect();
            per_title(&mut tally, targets, async |tally: &mut Tally| {
                submit_title(ctx, openers, &group, tally).await
            })
            .await?;
        }
    }
    let total = tally.input;
    let submitted = tally.ok - tally.stopped;
    let stopped = if tally.stopped > 0 {
        format!("; stopped monitoring for {}", tally.stopped)
    } else {
        String::new()
    };
    let expectation = if total == tally.skipped {
        Expectation::NotApplicable
    } else if tally.failed == 0 {
        Expectation::Met(format!("submitted {submitted} of {total}{stopped}"))
    } else {
        Expectation::Unmet(format!(
            "submitted {submitted} of {total}{stopped}; {} failed ({} will be retried)",
            tally.failed, tally.retrying
        ))
    };
    let mut report = Report::new(total, tally.ok, tally.failed, tally.skipped, expectation)?
        .with_counter("submitted", submitted)
        .with_counter("stopped", tally.stopped)
        .with_counter("failed", tally.failed)
        .with_counter("retrying", tally.retrying);
    for item in tally.items {
        report = report.with_item(item);
    }
    Ok(report)
}

/// What the source reports for one unit of a title: its files, how many
/// are expected (aired episodes, the album's tracks) and its queue.
fn unit_files<'a>(
    files: &Files,
    queue: &'a [QueueEntry],
    unit: Option<UnitId>,
) -> (FileState, Option<u32>, Vec<&'a QueueEntry>) {
    match (files, unit) {
        (Files::Movie { .. }, _) | (Files::Units(_), None) => {
            (files.state(), None, queue.iter().collect())
        }
        (Files::Units(listing), Some(unit)) => {
            let progress = listing.items.iter().find(|u| u.unit == unit);
            (
                progress.map_or(FileState::None, UnitProgress::state),
                // An unknown unit counts as nothing expected: never complete.
                Some(progress.map_or(0, |p| p.expected)),
                queue.iter().filter(|e| e.unit == Some(unit)).collect(),
            )
        }
    }
}

/// Sync one title (all followed units of one title at one instance).
async fn sync_title(
    ctx: &JobContext,
    openers: Openers<'_>,
    group: &[SyncUnit],
    tally: &mut Tally,
    counts: &mut SyncCounts,
) -> Result<(), JobError> {
    let Some(first) = group.first() else {
        return Ok(());
    };
    let targets = || group.iter().map(SourceUnit::target);
    let fail_report = |tally: &mut Tally, code: &str, detail: &str| {
        for target in targets() {
            tally.push(
                target,
                ItemOutcome::Failed,
                Some(code),
                first.report_detail(detail),
            );
        }
    };
    let Some(conn) = ctx.db.service_connection(first.service_instance_id).await? else {
        instance_gone(tally, targets());
        return Ok(());
    };
    let client = match instance_client(openers, &conn) {
        Ok((_, client)) => client,
        Err(failure) => {
            fail_report(tally, failure.code, &failure.detail);
            return Ok(());
        }
    };
    let Some(source) = AnySource::new(&conn.adapter_type, client) else {
        fail_report(tally, CODE_ADAPTER_UNKNOWN, "not a source");
        return Ok(());
    };
    let key = match first.keys().natural_key() {
        Ok(key) => key,
        Err(reason) => {
            fail_report(tally, CODE_NO_NATURAL_KEY, &reason);
            return Ok(());
        }
    };
    let result = tokio::select! {
        result = source.progress(key) => result,
        () = ctx.cancel.cancelled() => return Err(JobError::Cancelled),
    };
    let now = Utc::now();
    let progress = match result {
        Ok(progress) => progress,
        Err(AdapterError::NotFound(_)) => {
            let reason = format!("the title is no longer in {}", conn.name);
            for unit in group {
                ctx.db
                    .fail_acquisition(unit.acquisition_id, "not_found", &reason, SYNC, now)
                    .await?;
            }
            fail_report(tally, "not_found", &reason);
            return Ok(());
        }
        Err(err) => {
            let error = err.to_string();
            tracing::warn!(title = %first.log_title(), error = %LogTitle::new(first.category, &error), "reading acquisition progress failed");
            fail_report(tally, err.code(), &error);
            return Ok(());
        }
    };
    let presence = if first.keys().title_kind() == ItemKind::Artist {
        // Albums: by release group in the music consume services (spec
        // 0006 Q7 B), else Lidarr's files.
        let albums: Vec<Uuid> = group.iter().filter_map(|u| u.release_group).collect();
        ctx.db.album_presence(first.category, &albums).await?
    } else {
        ctx.db
            .title_presence(
                first.category,
                first.keys().title_kind(),
                first.tmdb_id,
                first.tvdb_id,
                first.imdb_id.as_deref(),
            )
            .await?
    };
    for unit in group {
        let (files, aired, queue) =
            unit_files(&progress.files, &progress.queue.items, unit.keys().unit());
        let queue: Vec<QueueEntry> = queue.into_iter().cloned().collect();
        let observed = rules::observe(&Observation {
            files,
            aired,
            queue: &queue,
            consume: presence.unit(unit.season_number, unit.release_group),
        });
        let change = ctx
            .db
            .record_observed(unit.acquisition_id, &observed, SYNC, now)
            .await?;
        counts.add(change, &observed, unit.arriving_since, now);
        let detail = match change {
            Some((from, to)) => format!("{} → {}", from.as_str(), to.as_str()),
            None => observed.state.as_str().to_owned(),
        };
        tally.push(unit.target(), ItemOutcome::Succeeded, None, detail);
    }
    Ok(())
}

/// Look at the available units of one title that the consume services
/// list only as gone (spec 0004 R4): those without a file in the source
/// (or whose title left the source) end as removed; the others stay.
async fn removal_title(
    ctx: &JobContext,
    openers: Openers<'_>,
    group: &[SyncUnit],
    tally: &mut Tally,
    counts: &mut SyncCounts,
) -> Result<(), JobError> {
    let Some(first) = group.first() else {
        return Ok(());
    };
    let targets = || group.iter().map(SourceUnit::target);
    let fail_report = |tally: &mut Tally, code: &str, detail: &str| {
        for target in targets() {
            tally.push(
                target,
                ItemOutcome::Failed,
                Some(code),
                first.report_detail(detail),
            );
        }
    };
    let Some(conn) = ctx.db.service_connection(first.service_instance_id).await? else {
        instance_gone(tally, targets());
        return Ok(());
    };
    let client = match instance_client(openers, &conn) {
        Ok((_, client)) => client,
        Err(failure) => {
            fail_report(tally, failure.code, &failure.detail);
            return Ok(());
        }
    };
    let Some(source) = AnySource::new(&conn.adapter_type, client) else {
        fail_report(tally, CODE_ADAPTER_UNKNOWN, "not a source");
        return Ok(());
    };
    let key = match first.keys().natural_key() {
        Ok(key) => key,
        Err(reason) => {
            fail_report(tally, CODE_NO_NATURAL_KEY, &reason);
            return Ok(());
        }
    };
    let result = tokio::select! {
        result = source.progress(key) => result,
        () = ctx.cancel.cancelled() => return Err(JobError::Cancelled),
    };
    let progress = match result {
        Ok(progress) => Some(progress),
        Err(AdapterError::NotFound(_)) => None,
        Err(err) => {
            fail_report(tally, err.code(), &err.to_string());
            return Ok(());
        }
    };
    let now = Utc::now();
    for unit in group {
        let target = unit.target();
        let files = progress.as_ref().map_or(FileState::None, |p| {
            unit_files(&p.files, &p.queue.items, unit.keys().unit()).0
        });
        if files != FileState::None {
            ctx.db.record_removal_kept(unit.acquisition_id, now).await?;
            tally.push(
                target,
                ItemOutcome::Succeeded,
                None,
                format!("gone from the library but {} has files: kept", conn.name),
            );
            continue;
        }
        if ctx
            .db
            .record_removed(unit.acquisition_id, SYNC, now)
            .await?
        {
            counts.removed += 1;
        }
        tally.push(
            target,
            ItemOutcome::Succeeded,
            None,
            "removed: no longer in the library".to_owned(),
        );
    }
    Ok(())
}

/// Ask Whisparr about the files of the available scenes of one title
/// (spec 0007 Q5 A, 0004 R4): a file keeps them, no file (or the scene no
/// longer in Whisparr) on two checks removes them.
async fn scene_file_title(
    ctx: &JobContext,
    openers: Openers<'_>,
    group: &[SyncUnit],
    tally: &mut Tally,
    counts: &mut SyncCounts,
) -> Result<(), JobError> {
    let Some(first) = group.first() else {
        return Ok(());
    };
    let targets = || group.iter().map(SourceUnit::target);
    let fail_report = |tally: &mut Tally, code: &str, detail: &str| {
        for target in targets() {
            tally.push(
                target,
                ItemOutcome::Failed,
                Some(code),
                first.report_detail(detail),
            );
        }
    };
    let Some(conn) = ctx.db.service_connection(first.service_instance_id).await? else {
        instance_gone(tally, targets());
        return Ok(());
    };
    let client = match instance_client(openers, &conn) {
        Ok((_, client)) => client,
        Err(failure) => {
            fail_report(tally, failure.code, &failure.detail);
            return Ok(());
        }
    };
    let Some(source) = AnySource::new(&conn.adapter_type, client) else {
        fail_report(tally, CODE_ADAPTER_UNKNOWN, "not a source");
        return Ok(());
    };
    let key = match first.keys().natural_key() {
        Ok(key) => key,
        Err(reason) => {
            fail_report(tally, CODE_NO_NATURAL_KEY, &reason);
            return Ok(());
        }
    };
    // Only Whisparr answering that the scene is not in its library counts
    // as "gone"; any error (a 404 from a wrong address, an unknown shape)
    // is reported and records nothing: Whisparr is the only evidence.
    let result = tokio::select! {
        result = scene_has_file(&source, key) => result,
        () = ctx.cancel.cancelled() => return Err(JobError::Cancelled),
    };
    let has_file = match result {
        Ok(has_file) => has_file,
        Err(err) => {
            fail_report(tally, err.code(), &err.to_string());
            return Ok(());
        }
    };
    let now = Utc::now();
    for unit in group {
        let outcome = ctx
            .db
            .record_scene_file(unit.acquisition_id, has_file, SYNC, now)
            .await?;
        let detail = match outcome {
            SceneFile::Kept => format!("{} has the file: available", conn.name),
            SceneFile::Missing => format!(
                "{} shows no file: removed if it still shows none at the next check",
                conn.name
            ),
            SceneFile::Removed => {
                counts.removed += 1;
                "removed: no longer in the library".to_owned()
            }
            SceneFile::NotAvailable => "no longer available: nothing to check".to_owned(),
        };
        tally.push(unit.target(), ItemOutcome::Succeeded, None, detail);
    }
    Ok(())
}

/// Whether the source has the file of scene `key`: `false` only when it
/// lists the scene without a file or does not list it at all.
async fn scene_has_file(source: &AnySource, key: NaturalKey) -> Result<bool, AdapterError> {
    match source.lookup(key).await? {
        Lookup::NotInLibrary => Ok(false),
        Lookup::InLibrary(_) => {
            Ok(source.progress(key).await?.files.state() == FileState::Complete)
        }
    }
}

/// What one `acquisitions.sync` run counted beyond the tally.
#[derive(Default)]
struct SyncCounts {
    /// Available units that left the library (removed).
    removed: u64,
    /// Units that became available.
    available: u64,
    /// Units in the source but not in the consume services yet.
    arriving: u64,
    /// Of those, units that started arriving within the refresh window.
    arriving_recently: u64,
}

impl SyncCounts {
    /// Count one observed unit; `since` is when it started arriving as of
    /// the previous sync (`None`: it just did).
    fn add(
        &mut self,
        change: Option<(AcquisitionState, AcquisitionState)>,
        observed: &Observed,
        since: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) {
        if matches!(change, Some((_, AcquisitionState::Available))) {
            self.available += 1;
        }
        if observed.refresh_library() {
            self.arriving += 1;
            if since.is_none_or(|since| now - since < LIBRARY_REFRESH_WINDOW) {
                self.arriving_recently += 1;
            }
        }
    }
}

/// Ask for an `inventory.sync` of every consume instance: right away, but
/// not before [`LIBRARY_REFRESH_INTERVAL`] after the last run. The kind's
/// unique key keeps it at one pending job (and pulls the scheduled one
/// forward); the "gone after two complete runs" rule is the inventory's
/// and stays untouched. The run shows as scheduled (trigger `schedule`),
/// like the hourly one it pulls forward.
async fn refresh_library(ctx: &JobContext, now: DateTime<Utc>) -> Result<(), JobError> {
    let kind = JobKind::InventorySync;
    let run_after = match ctx.db.last_run_started(kind.name()).await? {
        Some(last) => (last + LIBRARY_REFRESH_INTERVAL).max(now),
        None => now,
    };
    let key = kind.unique_key();
    ctx.db
        .enqueue_job(
            &NewJob {
                kind: kind.name(),
                payload: "{}",
                unique_key: Some(&key),
                trigger: Trigger::Schedule,
                run_after,
                max_attempts: kind.spec().max_attempts,
            },
            now,
        )
        .await?;
    Ok(())
}

pub(crate) async fn sync(ctx: &JobContext, openers: Openers<'_>) -> Result<Report, JobError> {
    let units = ctx.db.synced_acquisitions().await?;
    let mut tally = Tally::default();
    let mut counts = SyncCounts::default();
    for group in groups(units, |u| (u.service_instance_id, u.title_item_id)) {
        if ctx.cancel.is_cancelled() {
            return Err(JobError::Cancelled);
        }
        let targets = group.iter().map(SourceUnit::target).collect();
        per_title(&mut tally, targets, async |tally: &mut Tally| {
            sync_title(ctx, openers, &group, tally, &mut counts).await
        })
        .await?;
    }
    let candidates = ctx.db.removal_candidates(Utc::now()).await?;
    for group in groups(candidates, |u| (u.service_instance_id, u.title_item_id)) {
        if ctx.cancel.is_cancelled() {
            return Err(JobError::Cancelled);
        }
        let targets = group.iter().map(SourceUnit::target).collect();
        per_title(&mut tally, targets, async |tally: &mut Tally| {
            removal_title(ctx, openers, &group, tally, &mut counts).await
        })
        .await?;
    }
    let scenes = ctx.db.scene_file_checks(Utc::now()).await?;
    for group in groups(scenes, |u| (u.service_instance_id, u.title_item_id)) {
        if ctx.cancel.is_cancelled() {
            return Err(JobError::Cancelled);
        }
        let targets = group.iter().map(SourceUnit::target).collect();
        per_title(&mut tally, targets, async |tally: &mut Tally| {
            scene_file_title(ctx, openers, &group, tally, &mut counts).await
        })
        .await?;
    }
    // Every observation is stored already: a failed request for the early
    // refresh must not fail (and repeat) the whole sync.
    let mut refresh_failed = 0u64;
    if counts.arriving_recently > 0
        && let Err(err) = refresh_library(ctx, Utc::now()).await
    {
        tracing::warn!(error = %err, "asking for an early inventory.sync failed");
        refresh_failed = 1;
    }
    let total = tally.input;
    let expectation = if total == tally.skipped {
        Expectation::NotApplicable
    } else if tally.failed == 0 {
        Expectation::Met(format!("synced {} of {total}", tally.ok))
    } else {
        Expectation::Unmet(format!(
            "synced {} of {total}; {} failed",
            tally.ok, tally.failed
        ))
    };
    let mut report = Report::new(total, tally.ok, tally.failed, tally.skipped, expectation)?
        .with_counter("synced", tally.ok)
        .with_counter("failed", tally.failed)
        .with_counter("became_available", counts.available)
        .with_counter("removed", counts.removed)
        .with_counter("arriving", counts.arriving)
        .with_counter("library_refresh_failed", refresh_failed);
    for item in tally.items {
        report = report.with_item(item);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_and_reports_never_name_a_private_title() {
        assert_eq!(
            LogTitle::new(Category::Adult, "Scene A").to_string(),
            "[adult]"
        );
        assert_eq!(LogTitle::new(Category::Movies, "Heat").to_string(), "Heat");
        let unit = StopUnit {
            acquisition_id: 12,
            title_item_id: 1,
            title: "Scene A".into(),
            unit_title: "Scene A".into(),
            season_number: None,
            tmdb_id: Some(1),
            tvdb_id: None,
            mb_artist: None,
            release_group: None,
            stash_scene: Some(uuid::Uuid::from_u128(7)),
            service_instance_id: 1,
            category: Category::Adult,
        };
        assert_eq!(unit.target(), "[adult] acquisition #12");
        assert_eq!(unit.log_title().to_string(), "[adult]");
        assert_eq!(
            unit.report_detail("Scene A failed"),
            "details withheld (private category)"
        );
        let movie = StopUnit {
            category: Category::Movies,
            title: "Heat".into(),
            ..unit
        };
        assert_eq!(movie.target(), "Heat");
        assert_eq!(movie.report_detail("Heat failed"), "Heat failed");
    }

    #[test]
    fn albums_are_addressed_by_their_artist_and_release_group() {
        let artist = uuid::Uuid::from_u128(1);
        let album = uuid::Uuid::from_u128(2);
        let keys = UnitKeys {
            season: None,
            release_group: Some(album),
            tmdb: None,
            tvdb: None,
            mb_artist: Some(artist),
            stash_scene: None,
        };
        assert_eq!(keys.unit(), Some(UnitId::ReleaseGroup(album)));
        assert_eq!(keys.natural_key(), Ok(NaturalKey::MbArtist(artist)));
        assert_eq!(keys.title_kind(), ItemKind::Artist);
        // Without the artist's MBID Lidarr cannot be asked: refused, not guessed.
        let orphan = UnitKeys {
            mb_artist: None,
            ..keys
        };
        assert_eq!(
            orphan.natural_key(),
            Err("the artist has no MusicBrainz ID, which Lidarr needs".to_owned())
        );
        // Movies and seasons keep their keys.
        let season = UnitKeys {
            season: Some(2),
            release_group: None,
            tmdb: Some(1396),
            tvdb: Some(81189),
            mb_artist: None,
            stash_scene: None,
        };
        assert_eq!(season.natural_key(), Ok(NaturalKey::TvdbSeries(81189)));
        assert_eq!(season.unit(), Some(UnitId::Season(2)));
    }

    #[test]
    fn scenes_are_addressed_by_their_stashdb_uuid_as_whole_items() {
        let scene = uuid::Uuid::from_u128(3);
        let keys = UnitKeys {
            season: None,
            release_group: None,
            tmdb: None,
            tvdb: None,
            mb_artist: None,
            stash_scene: Some(scene),
        };
        assert_eq!(keys.natural_key(), Ok(NaturalKey::StashScene(scene)));
        assert_eq!(keys.unit(), None, "a scene has no units");
        assert_eq!(keys.title_kind(), ItemKind::Scene);
        // The key never shows the UUID (logs, reasons).
        assert_eq!(NaturalKey::StashScene(scene).to_string(), "stash:[private]");
    }
}
