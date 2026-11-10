//! `follows.check` (spec 0003 D8, slice 7; spec 0006 slice 6; domain
//! model §6).
//!
//! Every hour, for every active follow last checked a day ago or never
//! (`follows::CHECK_INTERVAL`): read the series or artist from the
//! category's default catalog and request what is new in the member's
//! name with the member's request mode, exactly as the member would (D6;
//! after an earlier rejection an admin decides again, D10). Each follow is
//! thus checked daily, at its own time of day, so catalog reads spread
//! over the day (spec 0006 Q14: no synchronized polling of `MusicBrainz`).
//!
//! - A series: the seasons it lists with an air date after the follow's
//!   cutoff. Seasons that are available already or that the member has an
//!   open request for are left out. The cutoff moves past every new season
//!   handled, together with the request, so a season is acted on once.
//! - An artist: the release groups of the category's release types (spec
//!   0006 Q3 B; album and EP without secondary types by default) first
//!   released after the follow date. Albums the member has asked for
//!   before (whatever became of that) and albums available already are
//!   left out; the date stays, so an album announced later with an earlier
//!   date than another is still found. Albums are stored under the artist
//!   when they are requested, as on the artist page.
//! - A performer or studio (spec 0007 Q4, slice 5): the scenes it lists
//!   released after the follow date, oldest first, without those the
//!   member asked for before or that are available. Each is its own
//!   request (as from the performer page), at most the private category's
//!   weekly cap per follow (`follows::split_by_cap`, counted over the
//!   follow's own requests of the last 7 days); the rest stay new for a
//!   later run or the member's own request, show as "N more new scenes
//!   this week" on the follow and notify `follow_capped` once a week. The
//!   catalog reads go to the private category's `StashDB` only, and
//!   nothing about the follow is logged or reported beyond its id.
//!
//! Each follow is its own unit (lesson 15): a member who lost the
//! entitlement or was suspended is skipped and keeps the cutoff (their
//! seasons and albums come once they may request again), a catalog failure
//! fails only that follow and its next check tries again. Every follow
//! looked at is noted as checked, whatever came of it. The report says
//! "checked n follows, m new seasons, a new albums, s new scenes, k
//! requests created, c scenes over the weekly cap"; a run without a report
//! is a failure (ADR 0004).
//!
//! Requests created here carry a `request_event` with trigger `system`,
//! actor `follows.check` and the units as detail, and notify the member
//! (`follow_requested`).

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use starr_core::catalog::{
    Catalog, CatalogDetails, CatalogEntry, CatalogRef, ExternalId, MediaKind, Relevance,
};
use starr_core::categories::is_private;
use starr_core::follows;
use starr_core::jobs::{Expectation, ItemOutcome, JobKind, Report, ReportItem, Trigger};
use starr_core::members::{Category, MemberStatus, RequestMode};
use starr_core::requests::{self as rules, Availability};
use starr_db::catalog::{CatalogItem, ItemKind, Provenance};
use starr_db::follows::{Cutoff, FollowRecord};
use starr_db::jobs::NewJob;
use starr_db::requests::{
    Actor, EventTrigger, NewRequest, RequestFollow, SourceTarget, TitlePresence, Unit, UnitFacts,
};
use starr_db::{Database, DbError};
use uuid::Uuid;

use crate::catalog::CatalogOpenError;
use crate::handlers::SystemJobs;
use crate::{JobContext, JobError};

const CHECK: Actor<'static> = Actor {
    trigger: EventTrigger::System,
    name: JobKind::FollowsCheck.name(),
};

/// Language of recorded titles (D5: English in v1).
const LANGUAGE: &str = "en-US";
/// How long recorded catalog metadata is kept before it counts as stale
/// (as when a member requests).
const METADATA_TTL: TimeDelta = TimeDelta::days(30);

const CODE_NOT_ENTITLED: &str = "not_entitled";
const CODE_MEMBER_INACTIVE: &str = "member_inactive";
const CODE_CATEGORY_NOT_READY: &str = "category_not_ready";
const CODE_NO_TMDB_ID: &str = "no_tmdb_id";
const CODE_NO_TVDB_ID: &str = "no_tvdb_id";
const CODE_NO_MBID: &str = "no_mbid";
const CODE_NO_STASH_ID: &str = "no_stash_id";
const CODE_UNFOLLOWED: &str = "unfollowed";
const CODE_RETRY: &str = "retry";
const CODE_DATABASE: &str = "database";

/// What a category needs for follows: switched on, a default catalog and a
/// complete source.
struct Ready {
    catalog_instance: i64,
    catalog_adapter: String,
    /// The catalog's relevance; for music, the release types a follow
    /// asks for (spec 0006 Q3, default album and EP).
    relevance: Relevance,
    /// Performer and studio follows: automatic requests per follow and
    /// week (spec 0007 Q4), read anew every run.
    cap: u32,
    source: SourceTarget,
}

async fn ready(db: &Database, category: Category) -> Result<Option<Ready>, DbError> {
    // The private category takes requests only with its operator
    // statement (ADR 0014).
    let enabled = db.list_categories().await?.into_iter().any(|c| {
        c.category == category
            && if is_private(category) {
                c.enabled_with_statement()
            } else {
                c.enabled
            }
    });
    if !enabled {
        return Ok(None);
    }
    let (Some(catalog), Some(source)) = (
        db.default_catalog(category).await?,
        db.source_target(category).await?,
    ) else {
        return Ok(None);
    };
    // The private catalog's key is used only while its last check passed:
    // a key the check refused (spec 0007 Q14, write roles) stays unused, as
    // in the private area.
    if is_private(category)
        && !db
            .get_service(catalog.service_instance_id)
            .await?
            .is_some_and(|s| s.last_check_at.is_some() && s.last_error.is_none())
    {
        return Ok(None);
    }
    // Only music follows filter by it; a series follow never reads it.
    let relevance = match (category, catalog.relevance()) {
        (Category::Music, Ok(Some(relevance))) => relevance,
        (Category::Music, Ok(None)) => Relevance::music_default(),
        (Category::Music, Err(err)) => {
            tracing::error!(error = %err, "the music relevance cannot be read");
            return Ok(None);
        }
        _ => Relevance::default(),
    };
    Ok(Some(Ready {
        cap: catalog
            .follow_cap_per_week
            .unwrap_or(follows::DEFAULT_CAP_PER_WEEK),
        catalog_instance: catalog.service_instance_id,
        catalog_adapter: catalog.adapter_type,
        relevance,
        source,
    }))
}

/// How one follow went.
enum Outcome {
    /// Checked; `new` seasons or albums found, `requested` of them asked
    /// for.
    Checked {
        new: usize,
        requested: usize,
    },
    Skipped {
        code: &'static str,
        detail: String,
    },
    Failed {
        code: &'static str,
        detail: String,
    },
}

#[derive(Default)]
struct Tally {
    /// When the run started: every follow it looks at is noted as checked
    /// at this time, so its next check is due a day after the run that
    /// checked it, however far into the run it came (reviewer: a later
    /// stamp would push it an hour back every day).
    started: DateTime<Utc>,
    ok: u64,
    skipped: u64,
    failed: u64,
    new_seasons: u64,
    new_albums: u64,
    new_scenes: u64,
    requests: u64,
    seasons_requested: u64,
    albums_requested: u64,
    scenes_requested: u64,
    scenes_capped: u64,
    submit: bool,
    items: Vec<ReportItem>,
}

/// How the run report names a follow: member and title, or a private one
/// by its id only (spec 0007).
fn report_target(follow: &FollowRecord) -> String {
    if is_private(follow.category) {
        format!("[adult] follow #{}", follow.id)
    } else {
        format!("{}: {}", follow.member_username, follow.title)
    }
}

/// A report detail of `follow`: withheld for a private one.
fn report_detail(follow: &FollowRecord, detail: String) -> String {
    if is_private(follow.category) {
        "details withheld (private category)".to_owned()
    } else {
        detail
    }
}

pub(crate) async fn check(ctx: &JobContext, jobs: &SystemJobs) -> Result<Report, JobError> {
    let started = Utc::now();
    let follows = ctx
        .db
        .due_follows(started - follows::CHECK_INTERVAL)
        .await?;
    let mut categories: BTreeMap<&'static str, Option<Ready>> = BTreeMap::new();
    let mut tally = Tally {
        started,
        ..Tally::default()
    };
    for follow in &follows {
        if ctx.cancel.is_cancelled() {
            return Err(JobError::Cancelled);
        }
        let key = follow.category.as_str();
        if !categories.contains_key(key) {
            categories.insert(key, ready(&ctx.db, follow.category).await?);
        }
        let ready = categories.get(key).and_then(Option::as_ref);
        let outcome = match check_one(ctx, jobs, follow, ready, &mut tally).await {
            Ok(outcome) => outcome,
            // One follow's database trouble does not stop the others.
            Err(JobError::Db(err)) => {
                tracing::error!(follow = follow.id, error = %err, "follow check failed");
                Outcome::Failed {
                    code: CODE_DATABASE,
                    detail: err.to_string(),
                }
            }
            Err(other) => return Err(other),
        };
        let unit = match follow.kind {
            ItemKind::Artist => "album",
            ItemKind::Performer | ItemKind::Studio => "scene",
            _ => "season",
        };
        let target = report_target(follow);
        let (outcome, code, detail) = match outcome {
            Outcome::Checked { new, requested } => {
                tally.ok += 1;
                (
                    ItemOutcome::Succeeded,
                    None,
                    format!("{new} new {unit}(s), {requested} requested"),
                )
            }
            Outcome::Skipped { code, detail } => {
                tally.skipped += 1;
                noted(&ctx.db, follow, tally.started).await;
                (ItemOutcome::Skipped, Some(code), detail)
            }
            Outcome::Failed { code, detail } => {
                tally.failed += 1;
                noted(&ctx.db, follow, tally.started).await;
                (ItemOutcome::Failed, Some(code), detail)
            }
        };
        tally.items.push(ReportItem {
            target,
            outcome,
            code: code.map(str::to_owned),
            detail: Some(report_detail(follow, detail)),
        });
    }
    if tally.submit {
        enqueue_submit(&ctx.db).await?;
    }
    Ok(report(follows.len() as u64, tally)?)
}

/// The run report of a run that checked `checked` follows.
fn report(checked: u64, tally: Tally) -> Result<Report, starr_core::jobs::ReportError> {
    let summary = format!(
        "checked {} follows, {} new seasons, {} new albums, {} new scenes, {} requests created, \
         {} scenes over the weekly cap",
        checked,
        tally.new_seasons,
        tally.new_albums,
        tally.new_scenes,
        tally.requests,
        tally.scenes_capped
    );
    let expectation = if tally.failed == 0 {
        Expectation::Met(summary)
    } else {
        Expectation::Unmet(format!("{summary}; {} follows failed", tally.failed))
    };
    let mut report = Report::new(checked, tally.ok, tally.failed, tally.skipped, expectation)?
        .with_counter("follows_checked", checked)
        .with_counter("new_seasons", tally.new_seasons)
        .with_counter("new_albums", tally.new_albums)
        .with_counter("requests_created", tally.requests)
        .with_counter("seasons_requested", tally.seasons_requested)
        .with_counter("albums_requested", tally.albums_requested)
        .with_counter("new_scenes", tally.new_scenes)
        .with_counter("scenes_requested", tally.scenes_requested)
        .with_counter("scenes_capped", tally.scenes_capped);
    for item in tally.items {
        report = report.with_item(item);
    }
    Ok(report)
}

/// Note that the run looked at `follow` without getting anywhere (skipped
/// or failed), so its next check is a day later, as for any other. Best
/// effort: the report already says what happened.
async fn noted(db: &Database, follow: &FollowRecord, now: DateTime<Utc>) {
    if let Err(err) = db
        .follow_checked(follow.id, follow.member_id, None, now)
        .await
    {
        tracing::error!(follow = follow.id, error = %err, "noting a follow check failed");
    }
    // A check that got nowhere says nothing about this week's scenes: the
    // hint of an earlier check goes (never a notification).
    if matches!(follow.kind, ItemKind::Performer | ItemKind::Studio)
        && let Err(err) = db.note_follow_cap(follow, 0, 0, now).await
    {
        tracing::error!(follow = follow.id, error = %err, "clearing a follow hint failed");
    }
}

async fn check_one(
    ctx: &JobContext,
    jobs: &SystemJobs,
    follow: &FollowRecord,
    ready: Option<&Ready>,
    tally: &mut Tally,
) -> Result<Outcome, JobError> {
    let (mode, ready) = match precheck(&ctx.db, follow, ready).await? {
        Ok(checked) => checked,
        Err(outcome) => return Ok(outcome),
    };
    match (follow.kind, follow.cutoff) {
        (_, Cutoff::Seasons(through)) => {
            check_series(ctx, jobs, follow, mode, ready, through, tally).await
        }
        (ItemKind::Performer | ItemKind::Studio, Cutoff::Date(known_through)) => {
            check_scenes(ctx, jobs, follow, mode, ready, known_through, tally).await
        }
        (_, Cutoff::Date(known_through)) => {
            check_artist(ctx, jobs, follow, mode, ready, known_through, tally).await
        }
    }
}

async fn check_series(
    ctx: &JobContext,
    jobs: &SystemJobs,
    follow: &FollowRecord,
    mode: RequestMode,
    ready: &Ready,
    seasons_through: u32,
    tally: &mut Tally,
) -> Result<Outcome, JobError> {
    let db = &ctx.db;
    let Some(tmdb_id) = follow.tmdb_id else {
        return Ok(Outcome::Failed {
            code: CODE_NO_TMDB_ID,
            detail: "the series has no TMDB ID".to_owned(),
        });
    };
    let reference = CatalogRef {
        kind: MediaKind::Series,
        id: tmdb_id.to_string(),
    };
    let details = match read_title(ctx, jobs, ready, &reference).await? {
        Ok(details) => details,
        Err(outcome) => return Ok(outcome),
    };
    let new = follows::new_seasons(details.seasons(), seasons_through);
    tally.new_seasons += new.len() as u64;
    let now = tally.started;
    let Some(&through) = new.last() else {
        db.follow_checked(follow.id, follow.member_id, Some(seasons_through), now)
            .await?;
        return Ok(Outcome::Checked {
            new: 0,
            requested: 0,
        });
    };

    // Record the series with its new seasons (catalog items to request).
    let title = db
        .upsert_catalog_item(&details, &provenance(ready), now)
        .await?;
    if title.tvdb_id.is_none() {
        return Ok(Outcome::Failed {
            code: CODE_NO_TVDB_ID,
            detail: "the series has no TVDB ID, which the source needs".to_owned(),
        });
    }
    let units = wanted_seasons(db, follow, &title, &new).await?;
    if units.is_empty() {
        db.follow_checked(follow.id, follow.member_id, Some(through), now)
            .await?;
        return Ok(Outcome::Checked {
            new: new.len(),
            requested: 0,
        });
    }
    let created = request(
        db,
        follow,
        (mode, ready),
        (title.id, Cutoff::Seasons(through)),
        &units,
        now,
    )
    .await?;
    Ok(match created {
        Ok(submit) => {
            tally.requests += 1;
            tally.seasons_requested += units.len() as u64;
            tally.submit |= submit;
            Outcome::Checked {
                new: new.len(),
                requested: units.len(),
            }
        }
        Err(outcome) => outcome,
    })
}

async fn check_artist(
    ctx: &JobContext,
    jobs: &SystemJobs,
    follow: &FollowRecord,
    mode: RequestMode,
    ready: &Ready,
    known_through: NaiveDate,
    tally: &mut Tally,
) -> Result<Outcome, JobError> {
    let db = &ctx.db;
    let Some(mbid) = follow.mb_artist else {
        return Ok(Outcome::Failed {
            code: CODE_NO_MBID,
            detail: "the artist has no MusicBrainz ID".to_owned(),
        });
    };
    let reference = CatalogRef {
        kind: MediaKind::Artist,
        id: mbid.hyphenated().to_string(),
    };
    let details = match read_title(ctx, jobs, ready, &reference).await? {
        Ok(details) => details,
        Err(outcome) => return Ok(outcome),
    };
    let new = follows::new_albums(details.albums(), known_through, &ready.relevance);
    let now = tally.started;
    // Asked for once (by the follow or by hand), never again by the follow:
    // only the others count as new (the date cutoff stays).
    let asked = db.requested_albums(follow.member_id).await?;
    let fresh: Vec<&CatalogEntry> = new
        .iter()
        .copied()
        .filter(|a| {
            uuid::Uuid::parse_str(&a.item.id)
                .is_ok_and(|release_group| !asked.contains(&release_group))
        })
        .collect();
    tally.new_albums += fresh.len() as u64;
    if fresh.is_empty() {
        db.follow_checked(follow.id, follow.member_id, None, now)
            .await?;
        return Ok(Outcome::Checked {
            new: fresh.len(),
            requested: 0,
        });
    }

    // Record the artist and the new albums under it (as a request from the
    // artist page does).
    let provenance = provenance(ready);
    let artist = db.upsert_catalog_item(&details, &provenance, now).await?;
    let albums = db
        .upsert_albums(artist.id, &fresh, &provenance, now)
        .await?;
    let units = wanted_albums(db, follow, artist.id, &albums).await?;
    if units.is_empty() {
        db.follow_checked(follow.id, follow.member_id, None, now)
            .await?;
        return Ok(Outcome::Checked {
            new: fresh.len(),
            requested: 0,
        });
    }
    let created = request(
        db,
        follow,
        (mode, ready),
        (artist.id, follow.cutoff),
        &units,
        now,
    )
    .await?;
    Ok(match created {
        Ok(submit) => {
            tally.requests += 1;
            tally.albums_requested += units.len() as u64;
            tally.submit |= submit;
            Outcome::Checked {
                new: fresh.len(),
                requested: units.len(),
            }
        }
        Err(outcome) => outcome,
    })
}

async fn check_scenes(
    ctx: &JobContext,
    jobs: &SystemJobs,
    follow: &FollowRecord,
    mode: RequestMode,
    ready: &Ready,
    known_through: NaiveDate,
    tally: &mut Tally,
) -> Result<Outcome, JobError> {
    let db = &ctx.db;
    let Some(stash_id) = follow.stash_id else {
        return Ok(Outcome::Failed {
            code: CODE_NO_STASH_ID,
            detail: "the performer or studio has no StashDB ID".to_owned(),
        });
    };
    let reference = CatalogRef {
        kind: if follow.kind == ItemKind::Studio {
            MediaKind::Studio
        } else {
            MediaKind::Performer
        },
        id: stash_id.hyphenated().to_string(),
    };
    let details = match read_title(ctx, jobs, ready, &reference).await? {
        Ok(details) => details,
        Err(outcome) => return Ok(outcome),
    };
    let now = tally.started;
    // Asked for once (by the follow or by hand), never again by the follow;
    // available ones are not asked for either.
    let asked = db.requested_scenes(follow.member_id).await?;
    let mut wanted = Vec::new();
    for scene in follows::new_scenes(details.scenes(), known_through) {
        let Ok(uuid) = Uuid::parse_str(&scene.item.id) else {
            continue;
        };
        if !asked.contains(&uuid) && scene_wanted(db, follow, uuid).await? {
            wanted.push((uuid, scene));
        }
    }
    tally.new_scenes += wanted.len() as u64;
    let used = db
        .follow_requests_since(follow.id, now - follows::CAP_WINDOW)
        .await?;
    // Room this week: scenes requested meanwhile (by hand, or by another
    // follow of the member in this run) leave it to the next ones.
    let room = follows::split_by_cap(wanted.len(), used, ready.cap).request;
    let provenance = provenance(ready);
    let mut requested = 0usize;
    let mut tried = 0usize;
    for &(uuid, scene) in &wanted {
        if requested == room {
            break;
        }
        tried += 1;
        let item = db
            .upsert_catalog_item(&scene_details(scene, uuid), &provenance, now)
            .await?;
        let units = [Unit {
            catalog_item_id: item.id,
            season_number: None,
        }];
        match request(
            db,
            follow,
            (mode, ready),
            (item.id, follow.cutoff),
            &units,
            now,
        )
        .await?
        {
            Ok(submit) => {
                requested += 1;
                tally.submit |= submit;
            }
            Err(
                outcome @ Outcome::Skipped {
                    code: CODE_UNFOLLOWED,
                    ..
                },
            ) => {
                tally.requests += requested as u64;
                tally.scenes_requested += requested as u64;
                return Ok(outcome);
            }
            // Asked for or available meanwhile: no longer wanted.
            Err(_) => {}
        }
    }
    let left = wanted.len() - tried;
    tally.requests += requested as u64;
    tally.scenes_requested += requested as u64;
    tally.scenes_capped += left as u64;
    let over = u32::try_from(left).unwrap_or(u32::MAX);
    let week = follows::cap_week(follow.created_at, now);
    if !db
        .follow_checked(follow.id, follow.member_id, None, now)
        .await?
        || !db.note_follow_cap(follow, over, week, now).await?
    {
        return Ok(Outcome::Skipped {
            code: CODE_UNFOLLOWED,
            detail: "the member stopped following while the job ran".to_owned(),
        });
    }
    Ok(Outcome::Checked {
        new: wanted.len(),
        requested,
    })
}

/// Whether the member wants scene `uuid`: not theirs, not available. A
/// scene nobody recorded yet is wanted.
async fn scene_wanted(db: &Database, follow: &FollowRecord, uuid: Uuid) -> Result<bool, DbError> {
    let Some(item) = db
        .resolve_catalog_item(MediaKind::Scene, &[ExternalId::StashScene(uuid)])
        .await?
    else {
        return Ok(true);
    };
    Ok(db
        .unit_facts(item.id, follow.member_id)
        .await?
        .first()
        .is_none_or(|f| wanted(f, &TitlePresence::NoService)))
}

/// A scene from a performer's or studio's list as catalog details to
/// record (the list carries every field a scene page shows).
fn scene_details(scene: &CatalogEntry, uuid: Uuid) -> CatalogDetails {
    CatalogDetails {
        entry: scene.clone(),
        external_ids: vec![ExternalId::StashScene(uuid)],
        genres: Vec::new(),
        trailer: None,
        certifications: Vec::new(),
        watch_offers: Vec::new(),
        children: Vec::new(),
        facts: starr_core::catalog::TitleFacts::default(),
    }
}

fn provenance(ready: &Ready) -> Provenance<'_> {
    let now = Utc::now();
    Provenance {
        adapter_type: &ready.catalog_adapter,
        instance_id: ready.catalog_instance,
        fetched_at: now,
        expires_at: now + METADATA_TTL,
    }
}

/// Request `units` of title `title_id` for the follow's member, noting the
/// follow as checked at `now` with `cutoff`. Whether `requests.submit` has
/// work, or why nothing was created.
async fn request(
    db: &Database,
    follow: &FollowRecord,
    (mode, ready): (RequestMode, &Ready),
    (title_id, cutoff): (i64, Cutoff),
    units: &[Unit],
    now: DateTime<Utc>,
) -> Result<Result<bool, Outcome>, DbError> {
    let created = db
        .create_request(
            &NewRequest {
                member_id: follow.member_id,
                catalog_item_id: title_id,
                follow: RequestFollow::Found {
                    follow_id: follow.id,
                    cutoff,
                },
                units,
                mode,
                source: &ready.source,
            },
            CHECK,
            now,
        )
        .await;
    match created {
        Ok(created) => Ok(Ok(created.submit)),
        Err(DbError::FollowEnded) => Ok(Err(Outcome::Skipped {
            code: CODE_UNFOLLOWED,
            detail: "the member stopped following while the job ran".to_owned(),
        })),
        // The member asked for one of them meanwhile, or one became
        // available: the next check works out again which are still
        // wanted.
        Err(DbError::RequestOpen | DbError::AlreadyAvailable) => Ok(Err(Outcome::Skipped {
            code: CODE_RETRY,
            detail: "something new was requested or became available while the job ran; \
                     the next check asks for the rest"
                .to_owned(),
        })),
        Err(err) => Err(err),
    }
}

/// Whether the follow can be checked: an active, entitled member and a
/// ready category. Returns the member's request mode and the category, or
/// why not.
async fn precheck<'a>(
    db: &Database,
    follow: &FollowRecord,
    ready: Option<&'a Ready>,
) -> Result<Result<(RequestMode, &'a Ready), Outcome>, DbError> {
    if follow.member_status != MemberStatus::Active {
        return Ok(Err(Outcome::Skipped {
            code: CODE_MEMBER_INACTIVE,
            detail: format!("the member is {}", follow.member_status.as_str()),
        }));
    }
    let Some(mode) = request_mode(db, follow).await? else {
        return Ok(Err(Outcome::Skipped {
            code: CODE_NOT_ENTITLED,
            detail: format!(
                "the member may no longer request {}",
                follow.category.as_str()
            ),
        }));
    };
    let Some(ready) = ready else {
        return Ok(Err(Outcome::Skipped {
            code: CODE_CATEGORY_NOT_READY,
            detail: format!(
                "{} is switched off or lacks a catalog or source",
                follow.category.as_str()
            ),
        }));
    };
    Ok(Ok((mode, ready)))
}

/// The series or artist as the category's default catalog lists it now,
/// or why it could not be read.
async fn read_title(
    ctx: &JobContext,
    jobs: &SystemJobs,
    ready: &Ready,
    reference: &CatalogRef,
) -> Result<Result<CatalogDetails, Outcome>, JobError> {
    let catalog = match jobs.catalog(&ctx.db, ready.catalog_instance).await {
        Ok(catalog) => catalog,
        Err(CatalogOpenError::Db(err)) => return Err(err.into()),
        Err(CatalogOpenError::Unavailable { code, detail }) => {
            return Ok(Err(Outcome::Failed { code, detail }));
        }
        Err(other) => {
            return Ok(Err(Outcome::Failed {
                code: CODE_CATEGORY_NOT_READY,
                detail: other.to_string(),
            }));
        }
    };
    let details = tokio::select! {
        result = catalog.details(reference, LANGUAGE) => result,
        () = ctx.cancel.cancelled() => return Err(JobError::Cancelled),
    };
    Ok(details.map_err(|err| Outcome::Failed {
        code: err.code(),
        detail: err.to_string(),
    }))
}

/// Whether the member wants unit `f`: not asked for by them, not available.
fn wanted(f: &UnitFacts, presence: &starr_db::requests::TitlePresence) -> bool {
    !f.mine
        && rules::unit_availability(
            f.acquisition,
            f.open_requests,
            presence.unit(f.season_number, f.release_group),
            f.expected_episodes,
        ) != Availability::Available
}

/// The units of the `new` seasons of `title` to request for the member:
/// without those available already or with an open request of theirs.
async fn wanted_seasons(
    db: &Database,
    follow: &FollowRecord,
    title: &CatalogItem,
    new: &[u32],
) -> Result<Vec<Unit>, DbError> {
    let presence = db
        .title_presence(
            follow.category,
            ItemKind::Series,
            title.tmdb_id,
            title.tvdb_id,
            title.imdb_id.as_deref(),
        )
        .await?;
    let facts = db.unit_facts(title.id, follow.member_id).await?;
    Ok(facts
        .iter()
        .filter(|f| f.season_number.is_some_and(|n| new.contains(&n)))
        .filter(|f| wanted(f, &presence))
        .map(|f| Unit {
            catalog_item_id: f.catalog_item_id,
            season_number: f.season_number,
        })
        .collect())
}

/// The units of `albums` (stored under artist `artist_id`, or under the
/// artist they were first requested through) to request for the member:
/// without those available already or with an open request of theirs.
async fn wanted_albums(
    db: &Database,
    follow: &FollowRecord,
    artist_id: i64,
    albums: &[CatalogItem],
) -> Result<Vec<Unit>, DbError> {
    let ids: BTreeSet<i64> = albums.iter().map(|a| a.id).collect();
    let parents: BTreeSet<i64> = albums
        .iter()
        .map(|a| a.parent_id.unwrap_or(artist_id))
        .collect();
    let mut facts = Vec::new();
    for parent in parents {
        facts.extend(
            db.unit_facts(parent, follow.member_id)
                .await?
                .into_iter()
                .filter(|f| ids.contains(&f.catalog_item_id)),
        );
    }
    let release_groups: Vec<uuid::Uuid> = facts.iter().filter_map(|f| f.release_group).collect();
    let presence = db.album_presence(follow.category, &release_groups).await?;
    Ok(facts
        .iter()
        .filter(|f| wanted(f, &presence))
        .map(|f| Unit {
            catalog_item_id: f.catalog_item_id,
            season_number: None,
        })
        .collect())
}

/// The member's request mode in the follow's category, if still entitled.
async fn request_mode(
    db: &Database,
    follow: &FollowRecord,
) -> Result<Option<RequestMode>, DbError> {
    Ok(db
        .entitlements(follow.member_id)
        .await?
        .into_iter()
        .find(|e| e.category == follow.category && e.enabled)
        .map(|e| e.request_mode))
}

/// Enqueue `requests.submit` for what was approved right away
/// (deduplicated with a pending one).
async fn enqueue_submit(db: &Database) -> Result<(), DbError> {
    let now = Utc::now();
    let kind = JobKind::RequestsSubmit;
    let key = kind.unique_key();
    db.enqueue_job(
        &NewJob {
            kind: kind.name(),
            payload: "{}",
            unique_key: Some(&key),
            trigger: Trigger::Schedule,
            run_after: now,
            max_attempts: kind.spec().max_attempts,
        },
        now,
    )
    .await?;
    Ok(())
}
