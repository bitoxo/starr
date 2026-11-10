//! Requests and acquisitions (domain model §6, spec 0003 slice 4).
//!
//! Requests are personal: every query that lists them either filters by
//! the owner or returns the owner with each row (lesson 4). Acquisitions
//! are shared: one per unit (movie or season) and source instance, linked
//! from the approved items of every member who asked for that unit (D10).
//!
//! Decisions and states come from `starr_core::requests`; this module only
//! applies them in transactions. A request's stored `state` is rewritten
//! from its items whenever an item or a linked acquisition changes, and
//! every change of it is recorded as a `request_event` in the same
//! transaction.
//!
//! Removing requests (spec 0004) never deletes: items end as `cancelled`,
//! `revoked` or `removed` and keep their link; a request is only hidden
//! from its member's list. When the last open item of an acquisition ends,
//! [`release_acquisition`] applies the rule of
//! [`AcquisitionState::when_unwanted`]: cancel it (nothing goes out), queue
//! a stop for `requests.submit`, or leave it.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;
use starr_core::categories::{MinimumAvailability, is_private};
use starr_core::members::{Category, RequestMode};
use starr_core::requests::{
    self as rules, AcquisitionState, Admission, ConsumePresence, Decision, DecisionAction,
    DownloadStage, Observed, RequestState, Unwanted,
};

use uuid::Uuid;

use crate::catalog::ItemKind;
use crate::follows::Cutoff;
use crate::{Database, DbError, follows, notifications, ts};

/// Who caused a change of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventTrigger {
    Member,
    Admin,
    /// A background job.
    System,
}

impl EventTrigger {
    pub const ALL: [Self; 3] = [Self::Member, Self::Admin, Self::System];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Admin => "admin",
            Self::System => "system",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == raw)
    }
}

/// The origin of a change: trigger and the actor's name (username, or the
/// job kind for the system), kept as text.
#[derive(Debug, Clone, Copy)]
pub struct Actor<'a> {
    pub trigger: EventTrigger,
    pub name: &'a str,
}

/// The category's source at the time of approval, with the settings a new
/// acquisition takes along (D9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTarget {
    pub category: Category,
    pub service_instance_id: i64,
    pub quality_profile_id: i64,
    pub root_folder_path: String,
    pub minimum_availability: Option<MinimumAvailability>,
    /// Music (Lidarr): the metadata profile for a new artist.
    pub metadata_profile_id: Option<i64>,
}

/// One unit of a request: the movie, or one season.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unit {
    pub catalog_item_id: i64,
    pub season_number: Option<u32>,
}

/// What a new request does to the member's follow of the series or
/// artist (D8, spec 0006 slice 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestFollow {
    /// Nothing.
    No,
    /// "Also get new seasons" / "follow the artist": start following with
    /// this cutoff (the seasons known now, or today), or keep the active
    /// follow (a seasons cutoff moves up to it).
    Start { cutoff: Cutoff },
    /// Created by `follows.check` for the member's follow `follow_id`: a
    /// seasons cutoff moves up to `cutoff` with the request, a date stays.
    /// Refused with [`DbError::FollowEnded`] if the follow ended meanwhile.
    Found { follow_id: i64, cutoff: Cutoff },
}

/// A request to create.
#[derive(Debug, Clone, Copy)]
pub struct NewRequest<'a> {
    pub member_id: i64,
    /// The movie or the series.
    pub catalog_item_id: i64,
    pub follow: RequestFollow,
    pub units: &'a [Unit],
    /// The member's request mode in the category (D6).
    pub mode: RequestMode,
    pub source: &'a SourceTarget,
}

/// Result of [`Database::create_request`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Created {
    pub request_id: i64,
    pub state: RequestState,
    /// Some acquisition waits for `requests.submit`.
    pub submit: bool,
}

/// A request named together with its owner, for admin actions on someone
/// else's record (lesson 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecisionTarget {
    pub request_id: i64,
    pub member_id: i64,
}

/// Result of an approval or rejection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecisionOutcome {
    /// Requests whose pending items were decided.
    pub decided: Vec<i64>,
    /// Requests without pending items (decided or withdrawn meanwhile).
    pub skipped: Vec<i64>,
    /// Some acquisition waits for `requests.submit`.
    pub submit: bool,
}

/// Result of [`Database::cancel_request`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// No such request of this member.
    NotFound,
    /// Nothing the member can still cancel (downloading, available, or
    /// ended already).
    NotCancellable,
    Cancelled {
        state: RequestState,
        /// A stop waits for `requests.submit`.
        stop: bool,
    },
}

/// Result of [`Database::revoke_requests`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RevokeOutcome {
    /// Requests whose open items were revoked.
    pub revoked: Vec<i64>,
    /// Requests without open items (ended meanwhile).
    pub skipped: Vec<i64>,
    /// A stop waits for `requests.submit`.
    pub stop: bool,
}

/// Result of [`Database::hide_request`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HideOutcome {
    /// No such request of this member.
    NotFound,
    /// Only finished requests can be hidden ([`RequestState::can_hide`]).
    NotFinished,
    Done,
}

/// What Starr changed in the source when it submitted an acquisition: what
/// a later stop may turn off again (spec 0004 R1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceChange {
    /// Starr added the title.
    pub added: bool,
    /// Starr added the title or switched the unit's monitoring on.
    pub monitored: bool,
}

/// Result of [`Database::withdraw_request`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithdrawOutcome {
    /// No such request of this member.
    NotFound,
    /// Nothing pending left to withdraw (D11: only until approved).
    NotPending,
    Withdrawn(RequestState),
}

/// Result of [`Database::retry_acquisition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryOutcome {
    /// Pending again; the id of the acquisition that will be submitted.
    Retried(i64),
    /// Unknown or not failed.
    NotFailed,
    /// Its source was removed and the category has no source to move to.
    NoSource,
}

/// The live state of an acquisition, as items show it.
#[derive(Debug, Clone, PartialEq)]
pub struct AcquisitionBrief {
    pub id: i64,
    pub state: AcquisitionState,
    pub progress: Option<f64>,
    pub source_message: Option<String>,
    pub last_error: Option<String>,
    /// Downloading only: importing, or arriving in the consume service.
    pub stage: Option<DownloadStage>,
    /// The category's consume services by name (" / " between several).
    pub library_name: Option<String>,
    /// The source instance's name; `None` once it was removed.
    pub service_name: Option<String>,
    /// Pending at a source whose writes are paused.
    pub held: bool,
    pub created_at: DateTime<Utc>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub available_at: Option<DateTime<Utc>>,
    /// When the current download stage began.
    pub stage_since: Option<DateTime<Utc>>,
}

/// The album a music request item asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemAlbum {
    pub release_group: Uuid,
    pub title: String,
    pub year: Option<i32>,
    /// Image path for the image proxy (`caa:<release group>`).
    pub poster_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RequestItemRecord {
    pub id: i64,
    pub catalog_item_id: i64,
    pub season_number: Option<u32>,
    /// Music: the album.
    pub album: Option<ItemAlbum>,
    pub decision: Decision,
    pub state: RequestState,
    pub decided_at: Option<DateTime<Utc>>,
    /// When it was withdrawn, cancelled, revoked or removed.
    pub ended_at: Option<DateTime<Utc>>,
    pub acquisition: Option<AcquisitionBrief>,
}

/// A request with its owner and items.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestRecord {
    pub id: i64,
    pub member_id: i64,
    pub member_username: String,
    pub member_display_name: String,
    pub catalog_item_id: i64,
    pub kind: ItemKind,
    pub title: String,
    pub year: Option<i32>,
    pub poster_path: Option<String>,
    pub tmdb_id: Option<u64>,
    /// Music: the artist's MBID.
    pub mb_artist: Option<Uuid>,
    /// Scenes: the `StashDB` UUID (private: never logged).
    pub stash_scene: Option<Uuid>,
    pub category: Category,
    pub state: RequestState,
    pub reject_reason: Option<String>,
    /// Why an admin revoked it.
    pub revoke_reason: Option<String>,
    /// The member hid it from their own list.
    pub hidden_at: Option<DateTime<Utc>>,
    pub follow: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub items: Vec<RequestItemRecord>,
}

/// Filter for [`Database::list_requests`].
#[allow(clippy::struct_excessive_bools)] // independent filters, all off by default
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestFilter {
    pub id: Option<i64>,
    pub member_id: Option<i64>,
    pub state: Option<RequestState>,
    pub category: Option<Category>,
    /// Only requests with items waiting for an admin ("Needs you").
    pub needs_decision: bool,
    /// Only requests with open (pending or approved) items.
    pub open: bool,
    /// Only requests for this title (the movie or the series).
    pub catalog_item_id: Option<i64>,
    /// Oldest first (the approval inbox) instead of newest first.
    pub oldest_first: bool,
    /// Leave out requests their member hid (the member's own list).
    pub without_hidden: bool,
    /// Leave out requests of this category (the adult category behind a
    /// closed privacy gate, spec 0007): filtered in SQL, not afterwards.
    pub without_category: Option<Category>,
    /// At most this many (default 500).
    pub limit: Option<u32>,
}

/// One change of a request's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestEvent {
    pub at: DateTime<Utc>,
    pub from_state: Option<RequestState>,
    pub to_state: RequestState,
    pub trigger: EventTrigger,
    pub actor: Option<String>,
    pub detail: Option<String>,
}

/// An acquisition as the admin overview shows it.
#[allow(clippy::struct_excessive_bools)] // independent facts about one acquisition
#[derive(Debug, Clone, PartialEq)]
pub struct AcquisitionRecord {
    pub id: i64,
    pub catalog_item_id: i64,
    /// The movie or the series.
    pub title_item_id: i64,
    pub title: String,
    pub year: Option<i32>,
    pub poster_path: Option<String>,
    pub season_number: Option<u32>,
    /// Music: the album (title and release group); the title is the
    /// artist's.
    pub album: Option<ItemAlbum>,
    pub category: Category,
    /// `None` once the source instance was removed.
    pub service_instance_id: Option<i64>,
    pub service_name: Option<String>,
    pub adapter_type: Option<String>,
    pub state: AcquisitionState,
    pub progress: Option<f64>,
    /// Downloading only: importing, or arriving in the consume service.
    pub stage: Option<DownloadStage>,
    /// The category's consume services by name (" / " between several).
    pub library_name: Option<String>,
    pub source_message: Option<String>,
    pub error_code: Option<String>,
    pub last_error: Option<String>,
    pub submit_attempts: u32,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub available_at: Option<DateTime<Utc>>,
    /// When the current download stage began.
    pub stage_since: Option<DateTime<Utc>>,
    pub synced_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Open (approved) request items linked to it (members sharing it).
    pub requests: u32,
    /// Pending at a source whose writes are paused: `requests.submit`
    /// leaves it alone until they are resumed.
    pub held: bool,
    /// Still followed by sync although nobody wants it any more (its
    /// download runs on, spec 0004 R1).
    pub unwanted: bool,
    /// A stop (unmonitor what Starr turned on) waits for `requests.submit`.
    pub stop_pending: bool,
    /// That stop is held: writes to the source are paused.
    pub stop_held: bool,
    /// When the stop went out.
    pub stopped_at: Option<DateTime<Utc>>,
}

/// An acquisition due for submission, with what the source needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitUnit {
    pub acquisition_id: i64,
    /// The movie, the series or the artist.
    pub title_item_id: i64,
    pub title: String,
    /// The unit's own catalog title (the album's, for music).
    pub unit_title: String,
    pub season_number: Option<u32>,
    pub tmdb_id: Option<u64>,
    pub tvdb_id: Option<u64>,
    /// Albums: the artist's MBID and the album's release group.
    pub mb_artist: Option<Uuid>,
    pub release_group: Option<Uuid>,
    /// Scenes: the `StashDB` UUID Whisparr keeps them by.
    pub stash_scene: Option<Uuid>,
    pub service_instance_id: i64,
    pub quality_profile_id: i64,
    pub root_folder_path: String,
    pub minimum_availability: Option<MinimumAvailability>,
    pub metadata_profile_id: Option<i64>,
    pub attempts: u32,
    /// Changes whenever the acquisition is restarted or retried.
    pub updated_at: DateTime<Utc>,
    /// Private categories are never named in logs or run reports.
    pub category: Category,
}

/// A stop due for `requests.submit`: unmonitor what Starr turned on for an
/// acquisition nobody wants any more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopUnit {
    pub acquisition_id: i64,
    /// The movie, the series or the artist.
    pub title_item_id: i64,
    pub title: String,
    pub unit_title: String,
    pub season_number: Option<u32>,
    pub tmdb_id: Option<u64>,
    pub tvdb_id: Option<u64>,
    pub mb_artist: Option<Uuid>,
    pub release_group: Option<Uuid>,
    pub stash_scene: Option<Uuid>,
    pub service_instance_id: i64,
    /// Private categories are never named in logs or run reports.
    pub category: Category,
}

/// What resuming a paused source sends out ([`Database::held_at`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    /// Acquisitions held there.
    pub total: u32,
    /// The first of them, oldest first.
    pub preview: Vec<HeldTitle>,
    /// Stops (stop monitoring) held there.
    pub stops: u32,
    /// The first of them, oldest first.
    pub stop_preview: Vec<HeldTitle>,
}

/// One held acquisition: the movie, the series and its season, or the
/// artist and the album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldTitle {
    pub title: String,
    pub year: Option<i64>,
    pub season: Option<u32>,
    /// Music: the album's title (the title is the artist's).
    pub album: Option<String>,
    /// The acquisition's category (private ones are shown neutrally).
    pub category: Category,
}

/// An acquisition `acquisitions.sync` follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncUnit {
    pub acquisition_id: i64,
    pub title_item_id: i64,
    pub title: String,
    pub unit_title: String,
    pub season_number: Option<u32>,
    pub tmdb_id: Option<u64>,
    pub tvdb_id: Option<u64>,
    pub imdb_id: Option<String>,
    pub mb_artist: Option<Uuid>,
    pub release_group: Option<Uuid>,
    pub stash_scene: Option<Uuid>,
    pub category: Category,
    pub service_instance_id: i64,
    pub state: AcquisitionState,
    /// Downloading and arriving since then (the last sync's observation).
    pub arriving_since: Option<DateTime<Utc>>,
}

/// What consume services have of a title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitlePresence {
    /// The category watches in no consume service.
    NoService,
    Absent,
    /// Present; series: episodes per season (merged by maximum).
    Present(BTreeMap<u32, u32>),
    /// Music ([`Database::album_presence`]): the present albums asked
    /// about, with their tracks (merged by maximum); albums not in the map
    /// are absent.
    Albums(BTreeMap<Uuid, u32>),
}

impl TitlePresence {
    /// Presence of one unit: the movie (both `None`), a season, or an
    /// album (by release group; its count is the tracks present).
    #[must_use]
    pub fn unit(&self, season: Option<u32>, release_group: Option<Uuid>) -> ConsumePresence {
        let found = |count: Option<&u32>| {
            count.map_or(ConsumePresence::Absent, |&episodes| {
                ConsumePresence::Present { episodes }
            })
        };
        match (self, season, release_group) {
            (Self::NoService, ..) => ConsumePresence::NoService,
            (Self::Absent, ..) | (Self::Albums(_), _, None) | (Self::Present(_), None, Some(_)) => {
                ConsumePresence::Absent
            }
            (Self::Albums(albums), _, Some(rg)) => found(albums.get(&rg)),
            (Self::Present(_), None, None) => ConsumePresence::Present { episodes: 1 },
            (Self::Present(seasons), Some(n), _) => found(seasons.get(&n)),
        }
    }
}

/// What is known about one unit of a title, for the member's status view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitFacts {
    pub catalog_item_id: i64,
    pub season_number: Option<u32>,
    /// Albums: the release group.
    pub release_group: Option<Uuid>,
    /// Season: episode count from the catalog; album: its track count.
    pub expected_episodes: Option<u32>,
    pub acquisition: Option<AcquisitionState>,
    /// The acquisition's download stage (downloading only).
    pub stage: Option<DownloadStage>,
    /// Anyone has an open (pending or approved) request for it.
    pub open_requests: bool,
    /// The asking member has one.
    pub mine: bool,
}

/// How long an album may be arriving (in the source, not listed by any
/// music consume service) before admins see it under "Needs you": the
/// time `acquisitions.sync` keeps asking for early inventory runs.
pub const UNLISTED_AFTER: chrono::TimeDelta = chrono::TimeDelta::hours(1);

/// An album stuck at "arriving" ([`Database::unlisted_albums`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlistedAlbum {
    pub acquisition_id: i64,
    pub artist: String,
    pub album: String,
    pub release_group: Option<Uuid>,
    /// The source instance's name (`None` if it was removed).
    pub source: Option<String>,
    /// The music consume services, " / " between several.
    pub libraries: Option<String>,
    /// Arriving since.
    pub since: DateTime<Utc>,
}

/// How long an available acquisition whose files the source still has is
/// left alone before `acquisitions.sync` asks the source again.
pub const REMOVAL_RECHECK: chrono::TimeDelta = chrono::TimeDelta::hours(6);

/// Detail of the event of a request whose unit left the library.
pub const REMOVED_DETAIL: &str = "no longer in the library";

/// Result of [`Database::record_scene_file`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneFile {
    /// Whisparr has the file.
    Kept,
    /// No file, for the first time: looked at again later.
    Missing,
    /// No file on two checks: removed, requestable again.
    Removed,
    /// Not available (any more): nothing recorded.
    NotAvailable,
}

/// Available acquisition `id` left the library: cancelled, its approved
/// items end as removed (spec 0004 R4). `false` if it was not available.
async fn remove_available(
    conn: &mut SqliteConnection,
    id: i64,
    actor: Actor<'_>,
    now: DateTime<Utc>,
) -> Result<bool, DbError> {
    let res = sqlx::query(
        "UPDATE acquisition SET state = 'cancelled', progress = NULL, source_message = NULL,
             download_stage = NULL, download_stage_since = NULL, stop_requested_at = NULL,
             next_attempt_at = NULL, file_missing_since = NULL, synced_at = ?1, updated_at = ?1
         WHERE id = ?2 AND state = 'available'",
    )
    .bind(ts(now))
    .bind(id)
    .execute(&mut *conn)
    .await?;
    if res.rows_affected() == 0 {
        return Ok(false);
    }
    let next = rules::decide(Decision::Approved, DecisionAction::Remove)
        .map_err(|e| DbError::InvalidData(e.to_string()))?;
    sqlx::query(
        "UPDATE request_item SET decision = ?1, ended_at = ?2
         WHERE acquisition_id = ?3 AND decision = 'approved'",
    )
    .bind(next.as_str())
    .bind(ts(now))
    .bind(id)
    .execute(&mut *conn)
    .await?;
    refresh_linked(conn, id, actor, Some(REMOVED_DETAIL), now).await?;
    Ok(true)
}

/// Inventory rows of the unit of `u` (a row of [`Database::removal_candidates`])
/// in the consume services of its category, matching `$condition`.
macro_rules! removal_inventory {
    ($condition:literal) => {
        concat!(
            "SELECT 1 FROM inventory i
             JOIN service_role r ON r.service_instance_id = i.service_instance_id
                  AND r.kind = 'consume' AND r.category = u.category
             WHERE i.kind = CASE WHEN u.release_group IS NOT NULL THEN 'album'
                                 WHEN u.season_number IS NULL THEN 'movie' ELSE 'series' END
               AND ",
            $condition,
            " AND ((u.tmdb_id IS NOT NULL AND i.tmdb_id = u.tmdb_id)
                   OR (u.tvdb_id IS NOT NULL AND i.tvdb_id = u.tvdb_id)
                   OR (u.imdb_id IS NOT NULL AND i.imdb_id = u.imdb_id)
                   OR (u.release_group IS NOT NULL
                       AND i.natural_key = 'album:mbrg:' || u.release_group))
               AND ",
            in_role_libraries!("r", "i")
        )
    };
}

/// Origin of changes to requests caused by another request sharing the
/// acquisition (never that member's or admin's name).
const RESTART: Actor<'static> = Actor {
    trigger: EventTrigger::System,
    name: "requests",
};

/// Detail of the first event of a request `follows.check` created for a
/// series follow (the seam for notifying the member, slice 8).
pub const FOUND_BY_FOLLOW: &str = "new season of a followed series";
/// The same for an artist follow (spec 0006 slice 6).
pub const FOUND_BY_ARTIST_FOLLOW: &str = "new album of a followed artist";
/// The same for a performer or studio follow (spec 0007 slice 5); names
/// neither.
pub const FOUND_BY_PRIVATE_FOLLOW: &str = "new scene of a followed performer or studio";

/// Error code of acquisitions failed because their source was removed.
pub const CODE_SOURCE_REMOVED: &str = "source_removed";

/// Fail the open acquisitions at service instance `instance_id` before the
/// instance is deleted (their `service_instance_id` becomes NULL), so the
/// requests sharing them show "failed" with the reason instead of a stale
/// state. A retry moves them to the category's current source.
pub(crate) async fn fail_acquisitions_at(
    conn: &mut SqliteConnection,
    instance_id: i64,
    instance_name: &str,
    now: DateTime<Utc>,
) -> Result<(), DbError> {
    let reason = format!("the source {instance_name} was removed from Starr");
    // Already failed ones were reported when they failed; cancelled ones
    // are nobody's any more.
    let newly: Vec<(i64,)> = sqlx::query_as(
        "SELECT id FROM acquisition WHERE service_instance_id = ?
             AND state NOT IN ('available', 'failed', 'cancelled')",
    )
    .bind(instance_id)
    .fetch_all(&mut *conn)
    .await?;
    let failed: Vec<(i64,)> = sqlx::query_as(
        "UPDATE acquisition SET state = 'failed', error_code = ?1, last_error = ?2,
             next_attempt_at = NULL, progress = NULL, source_message = NULL, download_stage = NULL, download_stage_since = NULL,
             stop_requested_at = NULL, updated_at = ?3
         WHERE service_instance_id = ?4 AND state NOT IN ('available', 'cancelled')
         RETURNING id",
    )
    .bind(CODE_SOURCE_REMOVED)
    .bind(&reason)
    .bind(ts(now))
    .bind(instance_id)
    .fetch_all(&mut *conn)
    .await?;
    let actor = Actor {
        trigger: EventTrigger::Admin,
        name: "services",
    };
    for (id,) in failed {
        if newly.contains(&(id,)) {
            notifications::acquisition_failed(conn, id, &reason, now).await?;
        }
        refresh_linked(conn, id, actor, Some(&reason), now).await?;
    }
    Ok(())
}

fn invalid(what: &'static str, raw: &str) -> DbError {
    crate::unknown(what, raw)
}

fn decision(raw: &str) -> Result<Decision, DbError> {
    Decision::parse(raw).ok_or_else(|| invalid("decision", raw))
}

fn acquisition_state(raw: &str) -> Result<AcquisitionState, DbError> {
    AcquisitionState::parse(raw).ok_or_else(|| invalid("acquisition state", raw))
}

/// The stored download stage as shown: only while downloading. A value this
/// version does not know (written by a newer one before a rollback) is
/// display data only, so it is ignored rather than failing whole lists.
fn download_stage(state: AcquisitionState, raw: Option<&str>) -> Option<DownloadStage> {
    let stored = raw.and_then(|raw| {
        let parsed = DownloadStage::parse(raw);
        if parsed.is_none() {
            tracing::warn!(stage = raw, "unknown download stage ignored");
        }
        parsed
    });
    DownloadStage::of(state, stored)
}

fn request_state(raw: &str) -> Result<RequestState, DbError> {
    RequestState::parse(raw).ok_or_else(|| invalid("request state", raw))
}

fn category(raw: &str) -> Result<Category, DbError> {
    Category::parse(raw).ok_or_else(|| invalid("category", raw))
}

fn minimum_availability(raw: Option<&str>) -> Result<Option<MinimumAvailability>, DbError> {
    raw.map(|raw| {
        [
            MinimumAvailability::Announced,
            MinimumAvailability::InCinemas,
            MinimumAvailability::Released,
        ]
        .into_iter()
        .find(|m| m.as_str() == raw)
        .ok_or_else(|| invalid("minimum availability", raw))
    })
    .transpose()
}

fn item_kind(raw: &str) -> Result<ItemKind, DbError> {
    ItemKind::parse(raw)
}

fn season(raw: Option<i64>) -> Option<u32> {
    raw.and_then(|n| u32::try_from(n).ok())
}

/// A stored MBID or `StashDB` UUID (`catalog_key` keeps them lower-case
/// and hyphenated).
fn mbid(raw: Option<&str>) -> Option<Uuid> {
    raw.and_then(|raw| Uuid::parse_str(raw).ok())
}

fn unsigned(raw: Option<i64>) -> Option<u64> {
    raw.and_then(|n| u64::try_from(n).ok())
}

fn count(raw: i64) -> u32 {
    u32::try_from(raw).unwrap_or(u32::MAX)
}

/// Whether acquisition `id` is still wanted: an approved item links to it.
async fn wanted(conn: &mut SqliteConnection, id: i64) -> Result<bool, DbError> {
    let (wanted,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM request_item
             WHERE acquisition_id = ? AND decision = 'approved')",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(wanted)
}

/// The last open item of acquisition `id` may have ended: if nobody wants
/// it any more, cancel it, queue a stop or leave it
/// ([`AcquisitionState::when_unwanted`], spec 0004 R1). Other members'
/// items are untouched. Returns whether a stop waits for `requests.submit`.
pub(crate) async fn release_acquisition(
    conn: &mut SqliteConnection,
    id: i64,
    now: DateTime<Utc>,
) -> Result<bool, DbError> {
    if wanted(conn, id).await? {
        return Ok(false);
    }
    let (raw_state, stored_stage, starr_monitored): (String, Option<String>, bool) =
        sqlx::query_as(
            "SELECT state, download_stage, starr_monitored FROM acquisition WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    let state = acquisition_state(&raw_state)?;
    match state.when_unwanted(
        download_stage(state, stored_stage.as_deref()),
        starr_monitored,
    ) {
        Unwanted::Cancel => {
            cancel_acquisition(conn, id, now).await?;
            Ok(false)
        }
        Unwanted::Stop => {
            sqlx::query(
                "UPDATE acquisition SET stop_requested_at = coalesce(stop_requested_at, ?1),
                     next_attempt_at = NULL, updated_at = ?1
                 WHERE id = ?2",
            )
            .bind(ts(now))
            .bind(id)
            .execute(&mut *conn)
            .await?;
            Ok(true)
        }
        Unwanted::Keep => Ok(false),
    }
}

/// Keep the pending stop of acquisition `id` only while the rule still
/// says stop for its current state and stage (files present: drop it).
/// Returns whether it is still due.
async fn keep_stop_if_due(
    conn: &mut SqliteConnection,
    id: i64,
    now: DateTime<Utc>,
) -> Result<bool, DbError> {
    let row: Option<(String, Option<String>, bool)> = sqlx::query_as(
        "SELECT state, download_stage, stop_requested_at IS NOT NULL FROM acquisition
         WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((raw_state, stored_stage, pending)) = row else {
        return Ok(false);
    };
    if !pending {
        return Ok(false);
    }
    let state = acquisition_state(&raw_state)?;
    // A pending stop means Starr switched the monitoring on.
    if state.when_unwanted(download_stage(state, stored_stage.as_deref()), true) == Unwanted::Stop {
        return Ok(true);
    }
    sqlx::query(
        "UPDATE acquisition SET stop_requested_at = NULL, next_attempt_at = NULL, updated_at = ?
         WHERE id = ?",
    )
    .bind(ts(now))
    .bind(id)
    .execute(&mut *conn)
    .await?;
    Ok(false)
}

/// Cancel acquisition `id`: nobody wants it, nothing is sent for it and
/// sync no longer follows it (the error of a failed one stays readable).
async fn cancel_acquisition(
    conn: &mut SqliteConnection,
    id: i64,
    now: DateTime<Utc>,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE acquisition SET state = 'cancelled', next_attempt_at = NULL,
             stop_requested_at = NULL, progress = NULL, source_message = NULL,
             download_stage = NULL, download_stage_since = NULL, updated_at = ?1
         WHERE id = ?2",
    )
    .bind(ts(now))
    .bind(id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// End the open items `items` (`id`, decision, acquisition id) with
/// `action` and release their acquisitions. Returns whether a stop waits
/// for `requests.submit`.
async fn end_items(
    conn: &mut SqliteConnection,
    items: &[(i64, Decision, Option<i64>)],
    action: DecisionAction,
    now: DateTime<Utc>,
) -> Result<bool, DbError> {
    let mut stop = false;
    for &(item_id, from, _) in items {
        let next = rules::decide(from, action).map_err(|e| DbError::InvalidData(e.to_string()))?;
        sqlx::query(
            "UPDATE request_item SET decision = ?1, ended_at = ?2,
                 decided_at = iif(?3, ?2, decided_at)
             WHERE id = ?4",
        )
        .bind(next.as_str())
        .bind(ts(now))
        // A withdrawal is the decision itself (D11).
        .bind(action == DecisionAction::Withdraw)
        .bind(item_id)
        .execute(&mut *conn)
        .await?;
    }
    let mut released: Vec<i64> = items.iter().filter_map(|(_, _, a)| *a).collect();
    released.sort_unstable();
    released.dedup();
    for acquisition in released {
        stop |= release_acquisition(conn, acquisition, now).await?;
    }
    Ok(stop)
}

/// The open items of request `id`: id, decision, and the linked
/// acquisition with its state.
async fn open_items(
    conn: &mut SqliteConnection,
    id: i64,
) -> Result<Vec<(i64, Decision, Option<(i64, AcquisitionState)>)>, DbError> {
    let rows: Vec<(i64, String, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT i.id, i.decision, a.id, a.state FROM request_item i
         LEFT JOIN acquisition a ON a.id = i.acquisition_id
         WHERE i.request_id = ? AND i.decision IN ('pending_approval', 'approved')
         ORDER BY i.id",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    rows.into_iter()
        .map(|(item, d, acquisition, state)| {
            let acquisition = match (acquisition, state) {
                (Some(a), Some(s)) => Some((a, acquisition_state(&s)?)),
                _ => None,
            };
            Ok((item, decision(&d)?, acquisition))
        })
        .collect()
}

/// Whether `member`'s next request for `unit` waits for an admin: after a
/// rejection (D10) or a revocation (spec 0004), unless the unit left the
/// library since ([`rules::needs_approval`]).
async fn needs_approval(
    conn: &mut SqliteConnection,
    member: i64,
    unit: i64,
) -> Result<bool, DbError> {
    let (refused, removed): (Option<DateTime<Utc>>, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT (SELECT max(coalesce(ended_at, decided_at)) FROM request_item
                 WHERE member_id = ?1 AND catalog_item_id = ?2
                   AND decision IN ('rejected', 'revoked')),
                (SELECT max(ended_at) FROM request_item
                 WHERE catalog_item_id = ?2 AND decision = 'removed')",
    )
    .bind(member)
    .bind(unit)
    .fetch_one(&mut *conn)
    .await?;
    Ok(rules::needs_approval(refused, removed))
}

/// Map the "one open request per member and unit" index to its rule.
fn open_conflict(err: sqlx::Error) -> DbError {
    if let sqlx::Error::Database(db) = &err
        && db.is_unique_violation()
    {
        return DbError::RequestOpen;
    }
    DbError::Sqlx(err)
}

/// Rewrite the stored state of request `id` from its items and record the
/// change as an event. `created` records the first event (from nothing).
async fn refresh_state(
    conn: &mut SqliteConnection,
    id: i64,
    actor: Actor<'_>,
    detail: Option<&str>,
    created: bool,
    now: DateTime<Utc>,
) -> Result<RequestState, DbError> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT i.decision, a.state FROM request_item i
         LEFT JOIN acquisition a ON a.id = i.acquisition_id
         WHERE i.request_id = ?",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let items = rows
        .iter()
        .map(|(d, a)| {
            Ok(rules::item_state(
                decision(d)?,
                a.as_deref().map(acquisition_state).transpose()?,
            ))
        })
        .collect::<Result<Vec<_>, DbError>>()?;
    let next = rules::request_state(&items);
    let (stored,): (String,) = sqlx::query_as("SELECT state FROM request WHERE id = ?")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    let before = request_state(&stored)?;
    if !created && before == next {
        return Ok(next);
    }
    sqlx::query("UPDATE request SET state = ?, updated_at = ? WHERE id = ?")
        .bind(next.as_str())
        .bind(ts(now))
        .bind(id)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO request_event (request_id, at, from_state, to_state, trigger, actor, detail)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(ts(now))
    .bind((!created).then_some(before.as_str()))
    .bind(next.as_str())
    .bind(actor.trigger.as_str())
    .bind(actor.name)
    .bind(detail)
    .execute(&mut *conn)
    .await?;
    notifications::request_transition(conn, id, (!created).then_some(before), next, now).await?;
    Ok(next)
}

/// Refresh every request with an item linked to acquisition `id`.
async fn refresh_linked(
    conn: &mut SqliteConnection,
    id: i64,
    actor: Actor<'_>,
    detail: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(), DbError> {
    refresh_linked_except(conn, id, None, actor, detail, now).await
}

/// [`refresh_linked`] without request `except` (the caller refreshes it).
async fn refresh_linked_except(
    conn: &mut SqliteConnection,
    id: i64,
    except: Option<i64>,
    actor: Actor<'_>,
    detail: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(), DbError> {
    let requests: Vec<(i64,)> = sqlx::query_as(
        "SELECT DISTINCT request_id FROM request_item
         WHERE acquisition_id = ? AND request_id IS NOT ? ORDER BY request_id",
    )
    .bind(id)
    .bind(except)
    .fetch_all(&mut *conn)
    .await?;
    for (request_id,) in requests {
        refresh_state(conn, request_id, actor, detail, false, now).await?;
    }
    Ok(())
}

async fn existing_acquisition(
    conn: &mut SqliteConnection,
    unit: i64,
    instance: i64,
) -> Result<Option<(i64, AcquisitionState)>, DbError> {
    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT id, state FROM acquisition WHERE catalog_item_id = ? AND service_instance_id = ?",
    )
    .bind(unit)
    .bind(instance)
    .fetch_optional(&mut *conn)
    .await?;
    row.map(|(id, state)| Ok((id, acquisition_state(&state)?)))
        .transpose()
}

/// The existing acquisition of `unit` at `source` as a new request sees
/// it: one nobody wants any more (revoked or cancelled, its download
/// running on) counts as cancelled, so the new request's mode decides
/// instead of attaching (spec 0004).
async fn admission_acquisition(
    conn: &mut SqliteConnection,
    unit: i64,
    source: &SourceTarget,
) -> Result<Option<(i64, AcquisitionState)>, DbError> {
    Ok(
        match existing_acquisition(conn, unit, source.service_instance_id).await? {
            Some((id, state))
                if state != AcquisitionState::Available && !wanted(conn, id).await? =>
            {
                Some((id, AcquisitionState::Cancelled))
            }
            other => other,
        },
    )
}

/// The acquisition of `unit` at the source: created as pending, a failed or
/// cancelled one started over (fresh attempts, current settings), as is one
/// whose monitoring Starr stopped while its download ran on (it needs its
/// monitoring back), any other one as it is (shared, D10).
async fn ensure_acquisition(
    conn: &mut SqliteConnection,
    unit: i64,
    source: &SourceTarget,
    now: DateTime<Utc>,
) -> Result<(i64, AcquisitionState), DbError> {
    let restarted: Option<(i64,)> = sqlx::query_as(
        "INSERT INTO acquisition (catalog_item_id, service_instance_id, category, state,
             quality_profile_id, root_folder_path, minimum_availability, metadata_profile_id,
             created_at, updated_at)
         VALUES (?1, ?2, ?3, 'pending', ?4, ?5, ?6, ?8, ?7, ?7)
         ON CONFLICT (catalog_item_id, service_instance_id) DO UPDATE SET
             state = 'pending', submit_attempts = 0, next_attempt_at = NULL,
             error_code = NULL, last_error = NULL, progress = NULL, source_message = NULL, download_stage = NULL, download_stage_since = NULL,
             starr_added = 0, starr_monitored = 0, stop_requested_at = NULL, stopped_at = NULL,
             file_missing_since = NULL,
             quality_profile_id = excluded.quality_profile_id,
             root_folder_path = excluded.root_folder_path,
             minimum_availability = excluded.minimum_availability,
             metadata_profile_id = excluded.metadata_profile_id,
             updated_at = excluded.updated_at
         WHERE state IN ('failed', 'cancelled')
            OR (stopped_at IS NOT NULL AND state <> 'available')
         RETURNING id",
    )
    .bind(unit)
    .bind(source.service_instance_id)
    .bind(source.category.as_str())
    .bind(source.quality_profile_id)
    .bind(&source.root_folder_path)
    .bind(source.minimum_availability.map(MinimumAvailability::as_str))
    .bind(ts(now))
    .bind(source.metadata_profile_id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((id,)) = restarted {
        return Ok((id, AcquisitionState::Pending));
    }
    existing_acquisition(conn, unit, source.service_instance_id)
        .await?
        .ok_or_else(|| DbError::InvalidData("acquisition vanished during approval".to_owned()))
}

/// Approve item `item_id` of request `request_id`: link it to its unit's
/// acquisition. Returns whether that acquisition waits for submission. A
/// failed acquisition that starts over changes the other requests sharing
/// it too; those are refreshed here.
async fn approve_item(
    conn: &mut SqliteConnection,
    request_id: i64,
    item_id: i64,
    unit: i64,
    source: &SourceTarget,
    actor: Actor<'_>,
    now: DateTime<Utc>,
) -> Result<bool, DbError> {
    let existing = existing_acquisition(conn, unit, source.service_instance_id).await?;
    let (acquisition, state) = match existing {
        // Became available meanwhile: link it, nothing to submit.
        Some((id, AcquisitionState::Available)) => (id, AcquisitionState::Available),
        _ => ensure_acquisition(conn, unit, source, now).await?,
    };
    if let Some((id, AcquisitionState::Failed)) = existing {
        // Another member's approval restarted it: a system event, without
        // that member's name, in the requests sharing it.
        let _ = actor;
        refresh_linked_except(
            conn,
            id,
            Some(request_id),
            RESTART,
            Some("acquisition restarted by a new approval"),
            now,
        )
        .await?;
    }
    sqlx::query(
        "UPDATE request_item SET decision = 'approved', decided_at = ?, acquisition_id = ?
         WHERE id = ?",
    )
    .bind(ts(now))
    .bind(acquisition)
    .bind(item_id)
    .execute(&mut *conn)
    .await?;
    // Wanted again before its stop went out: nothing to stop (nor the
    // error of a failed attempt to).
    sqlx::query(
        "UPDATE acquisition SET stop_requested_at = NULL,
             next_attempt_at = iif(state = 'pending', next_attempt_at, NULL),
             error_code = iif(state IN ('pending', 'failed'), error_code, NULL),
             last_error = iif(state IN ('pending', 'failed'), last_error, NULL)
         WHERE id = ? AND stop_requested_at IS NOT NULL",
    )
    .bind(acquisition)
    .execute(&mut *conn)
    .await?;
    Ok(state == AcquisitionState::Pending)
}

/// How long after an approval it can be undone (spec 0008 F-4), at most:
/// the toast offers it for seconds, this leaves room for a slow click.
pub const UNDO_APPROVAL_WINDOW: chrono::TimeDelta = chrono::TimeDelta::minutes(5);

/// The items of request `request_id` an admin approved since `now` minus
/// [`UNDO_APPROVAL_WINDOW`] (an automatic approval is never undone), with
/// their acquisition and whether taking the approval back sends nothing to
/// the source: the acquisition is held (pending at a paused source), or a
/// request outside `together` (the requests approved and undone together)
/// wants it too, so this approval sent nothing of its own. Files already
/// there do not count: that approval told the member "ready", which Starr
/// cannot take back.
async fn fresh_approvals(
    conn: &mut SqliteConnection,
    request_id: i64,
    together: &[i64],
    now: DateTime<Utc>,
) -> Result<Vec<(i64, Option<i64>, bool)>, DbError> {
    Ok(sqlx::query_as(
        "SELECT i.id, i.acquisition_id,
                a.id IS NOT NULL AND a.state NOT IN ('available', 'partially_available') AND (
                    (a.state = 'pending' AND s.writes_paused_at IS NOT NULL)
                    OR EXISTS (SELECT 1 FROM request_item o
                               WHERE o.acquisition_id = a.id AND o.decision = 'approved'
                                 AND o.request_id <> i.request_id
                                 AND o.request_id NOT IN (SELECT value FROM json_each(?3))))
         FROM request_item i
         LEFT JOIN acquisition a ON a.id = i.acquisition_id
         LEFT JOIN service_instance s ON s.id = a.service_instance_id
         WHERE i.request_id = ?1 AND i.decision = 'approved' AND i.decided_at >= ?2
           AND EXISTS (SELECT 1 FROM request_event e
                       WHERE e.request_id = i.request_id AND e.trigger = 'admin'
                         AND e.at = i.decided_at)
         ORDER BY i.id",
    )
    .bind(request_id)
    .bind(ts(now - UNDO_APPROVAL_WINDOW))
    .bind(serde_json::to_string(together)?)
    .fetch_all(&mut *conn)
    .await?)
}

/// Title, year, season number and album title of a held unit.
type HeldRow = (String, Option<i64>, Option<i64>, Option<String>, String);

/// `request_id, at, from_state, to_state, trigger, actor, detail` of a
/// request event.
type EventRow = (
    i64,
    DateTime<Utc>,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
);

/// `id, season_number, release group, metadata, acquisition state,
/// download stage, open, mine` of a unit.
type FactsRow = (
    i64,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
    bool,
);

#[derive(sqlx::FromRow)]
struct RequestRow {
    id: i64,
    member_id: i64,
    username: String,
    display_name: String,
    catalog_item_id: i64,
    kind: String,
    title: String,
    year: Option<i64>,
    poster_path: Option<String>,
    tmdb_id: Option<i64>,
    mb_artist: Option<String>,
    stash_scene: Option<String>,
    category: String,
    state: String,
    reject_reason: Option<String>,
    revoke_reason: Option<String>,
    hidden_at: Option<DateTime<Utc>>,
    follow: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct ItemRow {
    id: i64,
    request_id: i64,
    catalog_item_id: i64,
    season_number: Option<i64>,
    unit_kind: String,
    unit_title: String,
    unit_year: Option<i64>,
    unit_poster: Option<String>,
    release_group: Option<String>,
    decision: String,
    decided_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    acquisition_id: Option<i64>,
    acquisition_state: Option<String>,
    progress: Option<f64>,
    source_message: Option<String>,
    last_error: Option<String>,
    download_stage: Option<String>,
    library_name: Option<String>,
    service_name: Option<String>,
    writes_paused: Option<bool>,
    acquisition_created_at: Option<DateTime<Utc>>,
    submitted_at: Option<DateTime<Utc>>,
    available_at: Option<DateTime<Utc>>,
    download_stage_since: Option<DateTime<Utc>>,
}

impl ItemRow {
    fn into_record(self) -> Result<(i64, RequestItemRecord), DbError> {
        let decision = decision(&self.decision)?;
        let acquisition = match (
            self.acquisition_id,
            self.acquisition_state.as_deref(),
            self.acquisition_created_at,
        ) {
            (Some(id), Some(raw), Some(created_at)) => {
                let state = acquisition_state(raw)?;
                Some(AcquisitionBrief {
                    id,
                    state,
                    progress: self.progress,
                    source_message: self.source_message,
                    last_error: self.last_error,
                    stage: download_stage(state, self.download_stage.as_deref()),
                    library_name: self.library_name,
                    service_name: self.service_name,
                    held: state.is_held(self.writes_paused.unwrap_or(false)),
                    created_at,
                    submitted_at: self.submitted_at,
                    available_at: self.available_at,
                    stage_since: self.download_stage_since,
                })
            }
            _ => None,
        };
        let album = match (self.unit_kind.as_str(), mbid(self.release_group.as_deref())) {
            ("album", Some(release_group)) => Some(ItemAlbum {
                release_group,
                title: self.unit_title,
                year: self.unit_year.and_then(|y| i32::try_from(y).ok()),
                poster_path: self.unit_poster,
            }),
            _ => None,
        };
        Ok((
            self.request_id,
            RequestItemRecord {
                id: self.id,
                catalog_item_id: self.catalog_item_id,
                season_number: season(self.season_number),
                album,
                state: rules::item_state(decision, acquisition.as_ref().map(|a| a.state)),
                decision,
                decided_at: self.decided_at,
                ended_at: self.ended_at,
                acquisition,
            },
        ))
    }
}

#[derive(sqlx::FromRow)]
struct AcquisitionRow {
    id: i64,
    catalog_item_id: i64,
    title_item_id: i64,
    title: String,
    year: Option<i64>,
    poster_path: Option<String>,
    season_number: Option<i64>,
    unit_kind: String,
    unit_title: String,
    unit_year: Option<i64>,
    unit_poster: Option<String>,
    release_group: Option<String>,
    category: String,
    service_instance_id: Option<i64>,
    service_name: Option<String>,
    adapter_type: Option<String>,
    state: String,
    progress: Option<f64>,
    download_stage: Option<String>,
    library_name: Option<String>,
    source_message: Option<String>,
    error_code: Option<String>,
    last_error: Option<String>,
    submit_attempts: i64,
    next_attempt_at: Option<DateTime<Utc>>,
    submitted_at: Option<DateTime<Utc>>,
    available_at: Option<DateTime<Utc>>,
    download_stage_since: Option<DateTime<Utc>>,
    synced_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    requests: i64,
    writes_paused: bool,
    stop_requested_at: Option<DateTime<Utc>>,
    stopped_at: Option<DateTime<Utc>>,
}

impl TryFrom<AcquisitionRow> for AcquisitionRecord {
    type Error = DbError;

    fn try_from(row: AcquisitionRow) -> Result<Self, DbError> {
        let state = acquisition_state(&row.state)?;
        Ok(Self {
            id: row.id,
            catalog_item_id: row.catalog_item_id,
            title_item_id: row.title_item_id,
            title: row.title,
            year: row.year.and_then(|y| i32::try_from(y).ok()),
            poster_path: row.poster_path,
            season_number: season(row.season_number),
            album: match (row.unit_kind.as_str(), mbid(row.release_group.as_deref())) {
                ("album", Some(release_group)) => Some(ItemAlbum {
                    release_group,
                    title: row.unit_title,
                    year: row.unit_year.and_then(|y| i32::try_from(y).ok()),
                    poster_path: row.unit_poster,
                }),
                _ => None,
            },
            category: category(&row.category)?,
            service_instance_id: row.service_instance_id,
            service_name: row.service_name,
            adapter_type: row.adapter_type,
            state,
            progress: row.progress,
            stage: download_stage(state, row.download_stage.as_deref()),
            library_name: row.library_name,
            source_message: row.source_message,
            error_code: row.error_code,
            last_error: row.last_error,
            submit_attempts: count(row.submit_attempts),
            next_attempt_at: row.next_attempt_at,
            submitted_at: row.submitted_at,
            available_at: row.available_at,
            stage_since: row.download_stage_since,
            synced_at: row.synced_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
            requests: count(row.requests),
            held: state.is_held(row.writes_paused),
            unwanted: row.requests == 0 && state.is_synced(),
            stop_pending: row.stop_requested_at.is_some(),
            stop_held: row.stop_requested_at.is_some() && row.writes_paused,
            stopped_at: row.stopped_at,
        })
    }
}

/// The names of the media servers (consume services) of category `$category`
/// (an SQL expression), " / " between several, as one scalar subquery.
macro_rules! media_servers {
    ($category:literal) => {
        concat!(
            "(SELECT group_concat(si.name, ' / ' ORDER BY si.name)
              FROM service_role sr JOIN service_instance si ON si.id = sr.service_instance_id
              WHERE sr.category = ",
            $category,
            " AND sr.kind = 'consume')"
        )
    };
}

/// The request columns with owner and title; filter parameters ?1–?5 and
/// ?7–?9 as bound by [`Database::list_requests`].
macro_rules! request_select {
    () => {
        concat!(
            "SELECT r.id, r.member_id, m.username, m.display_name, r.catalog_item_id, c.kind,
                c.title, c.year, c.poster_path, ",
            tmdb_key!("c.id"),
            " AS tmdb_id, ",
            mb_artist_key!("c.id"),
            " AS mb_artist, ",
            stash_scene_key!("c.id"),
            " AS stash_scene, r.category, r.state,
                r.reject_reason, r.revoke_reason, r.hidden_at, r.follow, r.created_at,
                r.updated_at
         FROM request r
         JOIN member m ON m.id = r.member_id
         JOIN catalog_item c ON c.id = r.catalog_item_id
         WHERE (?1 IS NULL OR r.id = ?1) AND (?2 IS NULL OR r.member_id = ?2)
           AND (?3 IS NULL OR r.state = ?3) AND (?4 IS NULL OR r.category = ?4)
           AND (?5 = 0 OR EXISTS (SELECT 1 FROM request_item p WHERE p.request_id = r.id
                                  AND p.decision = 'pending_approval'))
           AND (?7 = 0 OR EXISTS (SELECT 1 FROM request_item o WHERE o.request_id = r.id
                                  AND o.decision IN ('pending_approval', 'approved')))
           AND (?8 IS NULL OR r.catalog_item_id = ?8)
           AND (?9 = 0 OR r.hidden_at IS NULL)
           AND (?10 IS NULL OR r.category <> ?10)"
        )
    };
}

/// The unit columns of an acquisition joined with its title: the movie
/// itself, the season's series, or the album's artist.
macro_rules! unit_select {
    ($columns:literal) => {
        concat!(
            "SELECT a.id AS acquisition_id, coalesce(p.id, c.id) AS title_item_id,
                    coalesce(p.title, c.title) AS title, c.title AS unit_title,
                    c.season_number, ",
            tmdb_key!("coalesce(p.id, c.id)"),
            " AS tmdb_id, ",
            tvdb_key!("coalesce(p.id, c.id)"),
            " AS tvdb_id, ",
            imdb_key!("coalesce(p.id, c.id)"),
            " AS imdb_id, ",
            mb_artist_key!("coalesce(p.id, c.id)"),
            " AS mb_artist, ",
            mb_release_group_key!("c.id"),
            " AS release_group, ",
            stash_scene_key!("c.id"),
            " AS stash_scene, ",
            $columns,
            " FROM acquisition a
              JOIN catalog_item c ON c.id = a.catalog_item_id
              LEFT JOIN catalog_item p ON p.id = c.parent_id"
        )
    };
}

#[derive(sqlx::FromRow)]
struct SubmitRow {
    acquisition_id: i64,
    title_item_id: i64,
    title: String,
    unit_title: String,
    season_number: Option<i64>,
    tmdb_id: Option<i64>,
    tvdb_id: Option<i64>,
    mb_artist: Option<String>,
    release_group: Option<String>,
    stash_scene: Option<String>,
    service_instance_id: i64,
    quality_profile_id: i64,
    root_folder_path: String,
    minimum_availability: Option<String>,
    metadata_profile_id: Option<i64>,
    submit_attempts: i64,
    updated_at: DateTime<Utc>,
    category: String,
}

#[derive(sqlx::FromRow)]
struct SyncRow {
    acquisition_id: i64,
    title_item_id: i64,
    title: String,
    unit_title: String,
    season_number: Option<i64>,
    tmdb_id: Option<i64>,
    tvdb_id: Option<i64>,
    imdb_id: Option<String>,
    mb_artist: Option<String>,
    release_group: Option<String>,
    stash_scene: Option<String>,
    category: String,
    service_instance_id: i64,
    state: String,
    download_stage: Option<String>,
    download_stage_since: Option<DateTime<Utc>>,
}

/// The note of a request `follows.check` created: which kind of follow
/// found it (naming nothing it follows).
fn found_by(new: &NewRequest<'_>) -> Option<&'static str> {
    match new.follow {
        RequestFollow::Found {
            cutoff: Cutoff::Seasons(_),
            ..
        } => Some(FOUND_BY_FOLLOW),
        RequestFollow::Found { .. } if is_private(new.source.category) => {
            Some(FOUND_BY_PRIVATE_FOLLOW)
        }
        RequestFollow::Found { .. } => Some(FOUND_BY_ARTIST_FOLLOW),
        RequestFollow::No | RequestFollow::Start { .. } => None,
    }
}

impl Database {
    /// Create a request with one item per unit, each admitted by the
    /// member's mode and the unit's acquisition at the source (D6, D10).
    /// Refused with [`DbError::RequestOpen`] if the member has an open
    /// request for one of the units, and with [`DbError::AlreadyAvailable`]
    /// if one is available through its acquisition.
    pub async fn create_request(
        &self,
        new: &NewRequest<'_>,
        actor: Actor<'_>,
        now: DateTime<Utc>,
    ) -> Result<Created, DbError> {
        if new.units.is_empty() {
            return Err(DbError::InvalidData("a request needs units".to_owned()));
        }
        let mut tx = self.begin_write().await?;
        let mut admitted = Vec::with_capacity(new.units.len());
        for unit in new.units {
            let open: Option<(i64,)> = sqlx::query_as(
                "SELECT 1 FROM request_item WHERE member_id = ? AND catalog_item_id = ?
                 AND decision IN ('pending_approval', 'approved')",
            )
            .bind(new.member_id)
            .bind(unit.catalog_item_id)
            .fetch_optional(&mut *tx)
            .await?;
            if open.is_some() {
                return Err(DbError::RequestOpen);
            }
            let existing = admission_acquisition(&mut tx, unit.catalog_item_id, new.source).await?;
            let admission = rules::admit(
                new.mode,
                existing.map(|(_, s)| s),
                needs_approval(&mut tx, new.member_id, unit.catalog_item_id).await?,
            );
            let Some(decision) = admission.decision() else {
                return Err(DbError::AlreadyAvailable);
            };
            admitted.push((unit, admission, decision));
        }

        let (request_id,): (i64,) = sqlx::query_as(
            "INSERT INTO request (member_id, catalog_item_id, category, state, follow,
                 follow_id, created_at, updated_at)
             VALUES (?, ?, ?, 'pending_approval', ?, ?, ?, ?) RETURNING id",
        )
        .bind(new.member_id)
        .bind(new.catalog_item_id)
        .bind(new.source.category.as_str())
        .bind(new.follow != RequestFollow::No)
        // The follow that created it: what its weekly cap counts (spec
        // 0007 Q4).
        .bind(match new.follow {
            RequestFollow::Found { follow_id, .. } => Some(follow_id),
            RequestFollow::No | RequestFollow::Start { .. } => None,
        })
        .bind(ts(now))
        .bind(ts(now))
        .fetch_one(&mut *tx)
        .await?;
        follows::link(&mut tx, new, now).await?;

        let mut submit = false;
        let mut attached = 0usize;
        for (unit, admission, decision) in &admitted {
            let (item_id,): (i64,) = sqlx::query_as(
                "INSERT INTO request_item (request_id, member_id, catalog_item_id, season_number,
                     decision)
                 VALUES (?, ?, ?, ?, 'pending_approval') RETURNING id",
            )
            .bind(request_id)
            .bind(new.member_id)
            .bind(unit.catalog_item_id)
            .bind(unit.season_number.map(i64::from))
            .fetch_one(&mut *tx)
            .await
            .map_err(open_conflict)?;
            if *decision == Decision::Approved {
                submit |= approve_item(
                    &mut tx,
                    request_id,
                    item_id,
                    unit.catalog_item_id,
                    new.source,
                    actor,
                    now,
                )
                .await?;
            }
            attached += usize::from(*admission == Admission::Attached);
        }
        let mut notes: Vec<String> = found_by(new).map(str::to_owned).into_iter().collect();
        if attached > 0 {
            notes.push(format!("{attached} attached to running acquisitions"));
        }
        let detail = (!notes.is_empty()).then(|| notes.join("; "));
        let state = refresh_state(&mut tx, request_id, actor, detail.as_deref(), true, now).await?;
        if matches!(new.follow, RequestFollow::Found { .. }) {
            notifications::found_by_follow(&mut tx, request_id, now).await?;
        }
        tx.commit().await?;
        Ok(Created {
            request_id,
            state,
            submit,
        })
    }

    /// Approve the pending items of `targets` (all or nothing): each is
    /// linked to its unit's acquisition at its category's source in
    /// `sources`. Refused with [`DbError::OwnerMismatch`] if a target names
    /// the wrong owner and [`DbError::CategorySourceMissing`] if a
    /// category has no entry in `sources`.
    pub async fn approve_requests(
        &self,
        targets: &[DecisionTarget],
        sources: &[SourceTarget],
        actor: Actor<'_>,
        now: DateTime<Utc>,
    ) -> Result<DecisionOutcome, DbError> {
        self.decide(targets, DecisionAction::Approve, sources, None, actor, now)
            .await
    }

    /// Reject the pending items of `targets` with `reason` (all or
    /// nothing). The reason is shown to the member; rejected items stay
    /// rejected (D10).
    pub async fn reject_requests(
        &self,
        targets: &[DecisionTarget],
        reason: &str,
        actor: Actor<'_>,
        now: DateTime<Utc>,
    ) -> Result<DecisionOutcome, DbError> {
        let reason = rules::reject_reason(reason)
            .ok_or_else(|| DbError::InvalidData("invalid reject reason".to_owned()))?;
        self.decide(
            targets,
            DecisionAction::Reject,
            &[],
            Some(reason),
            actor,
            now,
        )
        .await
    }

    /// Whether every one of `requests` has a fresh approval
    /// ([`UNDO_APPROVAL_WINDOW`]) that [`Self::undo_approvals`] can still
    /// take back at `now`: nothing went to the source for it.
    pub async fn approvals_undoable(
        &self,
        requests: &[i64],
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let mut conn = self.pool().acquire().await?;
        for id in requests {
            let fresh = fresh_approvals(&mut conn, *id, requests, now).await?;
            if fresh.is_empty() || fresh.iter().any(|f| !f.2) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Take fresh approvals of `targets` back (spec 0008 F-4; all or
    /// nothing): their items wait for approval again, acquisitions nobody
    /// else wants are released (a held one is cancelled, nothing went
    /// out), and the member's "approved" notification is withdrawn unless
    /// it was delivered somewhere already (an email that is on its way at
    /// that moment still arrives: the delivery job sends outside this
    /// transaction). Refused with
    /// [`DbError::UndoTooLate`] once something went to the source, the
    /// approval is older than [`UNDO_APPROVAL_WINDOW`] or there is none,
    /// and with [`DbError::OwnerMismatch`] for the wrong owner. A
    /// rejection is never undone (D11).
    pub async fn undo_approvals(
        &self,
        targets: &[DecisionTarget],
        actor: Actor<'_>,
        now: DateTime<Utc>,
    ) -> Result<DecisionOutcome, DbError> {
        let next = rules::decide(Decision::Approved, DecisionAction::UndoApprove)
            .map_err(|e| DbError::InvalidData(e.to_string()))?;
        let mut outcome = DecisionOutcome::default();
        let together: Vec<i64> = targets.iter().map(|t| t.request_id).collect();
        let mut tx = self.begin_write().await?;
        for target in targets {
            let owner: Option<(i64,)> =
                sqlx::query_as("SELECT member_id FROM request WHERE id = ?")
                    .bind(target.request_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            let Some((owner,)) = owner else {
                return Err(DbError::UndoTooLate);
            };
            if owner != target.member_id {
                return Err(DbError::OwnerMismatch);
            }
            let fresh = fresh_approvals(&mut tx, target.request_id, &together, now).await?;
            if fresh.is_empty() || fresh.iter().any(|f| !f.2) {
                return Err(DbError::UndoTooLate);
            }
            for (item, acquisition, _) in &fresh {
                sqlx::query(
                    "UPDATE request_item SET decision = ?, decided_at = NULL, acquisition_id = NULL
                     WHERE id = ?",
                )
                .bind(next.as_str())
                .bind(item)
                .execute(&mut *tx)
                .await?;
                if let Some(acquisition) = acquisition {
                    // Held or wanted by others: cancelled or kept, never a stop.
                    let stop = release_acquisition(&mut tx, *acquisition, now).await?;
                    debug_assert!(!stop, "an undone approval never queues a stop");
                }
            }
            sqlx::query(
                "DELETE FROM notification WHERE member_id = ? AND dedup_key = ?
                   AND NOT EXISTS (SELECT 1 FROM notification_delivery d
                                   WHERE d.notification_id = notification.id
                                     AND d.state = 'delivered')",
            )
            .bind(owner)
            .bind(format!("request:{}:request_approved", target.request_id))
            .execute(&mut *tx)
            .await?;
            refresh_state(
                &mut tx,
                target.request_id,
                actor,
                Some("approval undone"),
                false,
                now,
            )
            .await?;
            outcome.decided.push(target.request_id);
        }
        tx.commit().await?;
        Ok(outcome)
    }

    async fn decide(
        &self,
        targets: &[DecisionTarget],
        action: DecisionAction,
        sources: &[SourceTarget],
        reason: Option<&str>,
        actor: Actor<'_>,
        now: DateTime<Utc>,
    ) -> Result<DecisionOutcome, DbError> {
        let mut outcome = DecisionOutcome::default();
        let mut tx = self.begin_write().await?;
        for target in targets {
            let row: Option<(i64, String)> =
                sqlx::query_as("SELECT member_id, category FROM request WHERE id = ?")
                    .bind(target.request_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            let Some((owner, raw_category)) = row else {
                outcome.skipped.push(target.request_id);
                continue;
            };
            if owner != target.member_id {
                return Err(DbError::OwnerMismatch);
            }
            let pending: Vec<(i64, i64)> = sqlx::query_as(
                "SELECT id, catalog_item_id FROM request_item
                 WHERE request_id = ? AND decision = 'pending_approval' ORDER BY id",
            )
            .bind(target.request_id)
            .fetch_all(&mut *tx)
            .await?;
            if pending.is_empty() {
                outcome.skipped.push(target.request_id);
                continue;
            }
            let next = rules::decide(Decision::PendingApproval, action)
                .map_err(|e| DbError::InvalidData(e.to_string()))?;
            match next {
                Decision::Approved => {
                    let cat = category(&raw_category)?;
                    let source = sources
                        .iter()
                        .find(|s| s.category == cat)
                        .ok_or(DbError::CategorySourceMissing)?;
                    for (item_id, unit) in &pending {
                        outcome.submit |= approve_item(
                            &mut tx,
                            target.request_id,
                            *item_id,
                            *unit,
                            source,
                            actor,
                            now,
                        )
                        .await?;
                    }
                }
                other => {
                    sqlx::query(
                        "UPDATE request_item SET decision = ?, decided_at = ?
                         WHERE request_id = ? AND decision = 'pending_approval'",
                    )
                    .bind(other.as_str())
                    .bind(ts(now))
                    .bind(target.request_id)
                    .execute(&mut *tx)
                    .await?;
                    if let Some(reason) = reason {
                        sqlx::query("UPDATE request SET reject_reason = ? WHERE id = ?")
                            .bind(reason)
                            .bind(target.request_id)
                            .execute(&mut *tx)
                            .await?;
                    }
                }
            }
            let detail = match action {
                DecisionAction::Reject => reason.map(str::to_owned),
                _ => Some(format!("{} item(s)", pending.len())),
            };
            refresh_state(
                &mut tx,
                target.request_id,
                actor,
                detail.as_deref(),
                false,
                now,
            )
            .await?;
            outcome.decided.push(target.request_id);
        }
        tx.commit().await?;
        Ok(outcome)
    }

    /// Withdraw the pending items of the member's request `request_id`
    /// (D11). Approved items stay: they share an acquisition already.
    pub async fn withdraw_request(
        &self,
        member_id: i64,
        request_id: i64,
        actor: Actor<'_>,
        now: DateTime<Utc>,
    ) -> Result<WithdrawOutcome, DbError> {
        let mut tx = self.begin_write().await?;
        let owned: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM request WHERE id = ? AND member_id = ?")
                .bind(request_id)
                .bind(member_id)
                .fetch_optional(&mut *tx)
                .await?;
        if owned.is_none() {
            return Ok(WithdrawOutcome::NotFound);
        }
        let next = rules::decide(Decision::PendingApproval, DecisionAction::Withdraw)
            .map_err(|e| DbError::InvalidData(e.to_string()))?;
        let res = sqlx::query(
            "UPDATE request_item SET decision = ?1, decided_at = ?2, ended_at = ?2
             WHERE request_id = ?3 AND member_id = ?4 AND decision = 'pending_approval'",
        )
        .bind(next.as_str())
        .bind(ts(now))
        .bind(request_id)
        .bind(member_id)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            return Ok(WithdrawOutcome::NotPending);
        }
        let state = refresh_state(&mut tx, request_id, actor, None, false, now).await?;
        tx.commit().await?;
        Ok(WithdrawOutcome::Withdrawn(state))
    }

    /// Cancel what the member's request `request_id` still asks for and
    /// has not started downloading (spec 0004 R2): pending items are
    /// withdrawn, approved ones cancelled; items already downloading or
    /// available stay. Each acquisition left without open items is
    /// released ([`release_acquisition`]); other members' items are
    /// untouched.
    pub async fn cancel_request(
        &self,
        member_id: i64,
        request_id: i64,
        actor: Actor<'_>,
        now: DateTime<Utc>,
    ) -> Result<CancelOutcome, DbError> {
        let mut tx = self.begin_write().await?;
        let owned: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM request WHERE id = ? AND member_id = ?")
                .bind(request_id)
                .bind(member_id)
                .fetch_optional(&mut *tx)
                .await?;
        if owned.is_none() {
            return Ok(CancelOutcome::NotFound);
        }
        let items = open_items(&mut tx, request_id).await?;
        let mut withdraw = Vec::new();
        let mut cancel = Vec::new();
        for (item, decision, acquisition) in items {
            let ended = (item, decision, acquisition.map(|(id, _)| id));
            match rules::member_end(decision, acquisition.map(|(_, s)| s)) {
                Some(DecisionAction::Withdraw) => withdraw.push(ended),
                Some(DecisionAction::Cancel) => cancel.push(ended),
                _ => {}
            }
        }
        if withdraw.is_empty() && cancel.is_empty() {
            return Ok(CancelOutcome::NotCancellable);
        }
        let mut stop = end_items(&mut tx, &withdraw, DecisionAction::Withdraw, now).await?;
        stop |= end_items(&mut tx, &cancel, DecisionAction::Cancel, now).await?;
        let detail = format!("{} item(s) cancelled", withdraw.len() + cancel.len());
        let state = refresh_state(&mut tx, request_id, actor, Some(&detail), false, now).await?;
        tx.commit().await?;
        Ok(CancelOutcome::Cancelled { state, stop })
    }

    /// Revoke the open items of `targets` with `reason` (spec 0004 R3; all
    /// or nothing): pending and approved items end as revoked, whatever
    /// their acquisition's state; the member sees the reason and is
    /// notified. Each acquisition left without open items is released.
    /// Refused with [`DbError::OwnerMismatch`] if a target names the wrong
    /// owner.
    pub async fn revoke_requests(
        &self,
        targets: &[DecisionTarget],
        reason: &str,
        actor: Actor<'_>,
        now: DateTime<Utc>,
    ) -> Result<RevokeOutcome, DbError> {
        let reason = rules::reject_reason(reason)
            .ok_or_else(|| DbError::InvalidData("invalid revoke reason".to_owned()))?;
        let mut outcome = RevokeOutcome::default();
        let mut tx = self.begin_write().await?;
        for target in targets {
            let owner: Option<(i64,)> =
                sqlx::query_as("SELECT member_id FROM request WHERE id = ?")
                    .bind(target.request_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            let Some((owner,)) = owner else {
                outcome.skipped.push(target.request_id);
                continue;
            };
            if owner != target.member_id {
                return Err(DbError::OwnerMismatch);
            }
            if outcome.revoked.contains(&target.request_id) {
                continue;
            }
            let items: Vec<(i64, Decision, Option<i64>)> = open_items(&mut tx, target.request_id)
                .await?
                .into_iter()
                .map(|(item, decision, acquisition)| {
                    (item, decision, acquisition.map(|(id, _)| id))
                })
                .collect();
            if items.is_empty() {
                outcome.skipped.push(target.request_id);
                continue;
            }
            sqlx::query("UPDATE request SET revoke_reason = ? WHERE id = ?")
                .bind(reason)
                .bind(target.request_id)
                .execute(&mut *tx)
                .await?;
            outcome.stop |= end_items(&mut tx, &items, DecisionAction::Revoke, now).await?;
            refresh_state(&mut tx, target.request_id, actor, Some(reason), false, now).await?;
            outcome.revoked.push(target.request_id);
        }
        tx.commit().await?;
        Ok(outcome)
    }

    /// Hide the member's finished request `request_id` from their own list
    /// (`hidden`), or show it again. Admins and the audit log still see it.
    pub async fn hide_request(
        &self,
        member_id: i64,
        request_id: i64,
        hidden: bool,
        now: DateTime<Utc>,
    ) -> Result<HideOutcome, DbError> {
        let mut tx = self.begin_write().await?;
        let row: Option<(String,)> =
            sqlx::query_as("SELECT state FROM request WHERE id = ? AND member_id = ?")
                .bind(request_id)
                .bind(member_id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some((raw,)) = row else {
            return Ok(HideOutcome::NotFound);
        };
        if hidden && !request_state(&raw)?.can_hide() {
            return Ok(HideOutcome::NotFinished);
        }
        sqlx::query(
            "UPDATE request SET hidden_at = iif(?1, coalesce(hidden_at, ?2), NULL) WHERE id = ?3",
        )
        .bind(hidden)
        .bind(ts(now))
        .bind(request_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(HideOutcome::Done)
    }

    /// Requests with owner and items, newest first unless
    /// [`RequestFilter::oldest_first`].
    pub async fn list_requests(
        &self,
        filter: RequestFilter,
    ) -> Result<Vec<RequestRecord>, DbError> {
        let sql: &'static str = if filter.oldest_first {
            concat!(request_select!(), " ORDER BY r.created_at, r.id LIMIT ?6")
        } else {
            concat!(
                request_select!(),
                " ORDER BY r.created_at DESC, r.id DESC LIMIT ?6"
            )
        };
        let rows: Vec<RequestRow> = sqlx::query_as(sql)
            .bind(filter.id)
            .bind(filter.member_id)
            .bind(filter.state.map(RequestState::as_str))
            .bind(filter.category.map(Category::as_str))
            .bind(filter.needs_decision)
            .bind(i64::from(filter.limit.unwrap_or(500)))
            .bind(filter.open)
            .bind(filter.catalog_item_id)
            .bind(filter.without_hidden)
            .bind(filter.without_category.map(Category::as_str))
            .fetch_all(self.pool())
            .await?;
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        let items: Vec<ItemRow> = sqlx::query_as(concat!(
            "SELECT i.id, i.request_id, i.catalog_item_id, i.season_number,
                    u.kind AS unit_kind, u.title AS unit_title, u.year AS unit_year,
                    u.poster_path AS unit_poster, ",
            mb_release_group_key!("u.id"),
            " AS release_group, i.decision,
                    i.decided_at, i.ended_at, a.id AS acquisition_id, a.state AS acquisition_state,
                    a.progress, a.source_message, a.last_error, a.download_stage,
                    s.name AS service_name, s.writes_paused_at IS NOT NULL AS writes_paused,
                    a.created_at AS acquisition_created_at, a.submitted_at, a.available_at,
                    a.download_stage_since, ",
            media_servers!("a.category"),
            " AS library_name
             FROM request_item i JOIN catalog_item u ON u.id = i.catalog_item_id
             LEFT JOIN acquisition a ON a.id = i.acquisition_id
             LEFT JOIN service_instance s ON s.id = a.service_instance_id
             WHERE i.request_id IN (SELECT value FROM json_each(?))
             ORDER BY i.season_number, i.id"
        ))
        .bind(serde_json::to_string(&ids)?)
        .fetch_all(self.pool())
        .await?;
        let mut by_request: BTreeMap<i64, Vec<RequestItemRecord>> = BTreeMap::new();
        for (request_id, record) in crate::readable(items.into_iter().map(ItemRow::into_record))? {
            by_request.entry(request_id).or_default().push(record);
        }
        crate::readable(rows.into_iter().map(|row| {
            Ok(RequestRecord {
                items: by_request.remove(&row.id).unwrap_or_default(),
                id: row.id,
                member_id: row.member_id,
                member_username: row.username,
                member_display_name: row.display_name,
                catalog_item_id: row.catalog_item_id,
                kind: item_kind(&row.kind)?,
                title: row.title,
                year: row.year.and_then(|y| i32::try_from(y).ok()),
                poster_path: row.poster_path,
                tmdb_id: unsigned(row.tmdb_id),
                mb_artist: mbid(row.mb_artist.as_deref()),
                stash_scene: mbid(row.stash_scene.as_deref()),
                category: category(&row.category)?,
                state: request_state(&row.state)?,
                reject_reason: row.reject_reason,
                revoke_reason: row.revoke_reason,
                hidden_at: row.hidden_at,
                follow: row.follow,
                created_at: row.created_at,
                updated_at: row.updated_at,
            })
        }))
    }

    /// One request with owner and items.
    pub async fn get_request(&self, id: i64) -> Result<Option<RequestRecord>, DbError> {
        Ok(self
            .list_requests(RequestFilter {
                id: Some(id),
                limit: Some(1),
                ..RequestFilter::default()
            })
            .await?
            .pop())
    }

    /// The state changes of request `id`, oldest first.
    pub async fn request_events(&self, id: i64) -> Result<Vec<RequestEvent>, DbError> {
        Ok(self
            .requests_events(&[id])
            .await?
            .remove(&id)
            .unwrap_or_default())
    }

    /// The current state of requests `ids`; ids that do not exist are left out.
    pub async fn request_states(
        &self,
        ids: &[i64],
    ) -> Result<BTreeMap<i64, RequestState>, DbError> {
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT id, state FROM request WHERE id IN (SELECT value FROM json_each(?))",
        )
        .bind(serde_json::to_string(ids)?)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|(id, raw)| Ok((id, request_state(&raw)?)))
            .collect()
    }

    /// The state changes of requests `ids`, oldest first, by request.
    pub async fn requests_events(
        &self,
        ids: &[i64],
    ) -> Result<BTreeMap<i64, Vec<RequestEvent>>, DbError> {
        let rows: Vec<EventRow> = sqlx::query_as(
            "SELECT request_id, at, from_state, to_state, trigger, actor, detail
             FROM request_event
             WHERE request_id IN (SELECT value FROM json_each(?)) ORDER BY id",
        )
        .bind(serde_json::to_string(ids)?)
        .fetch_all(self.pool())
        .await?;
        let events = rows
            .into_iter()
            .map(|(request_id, at, from, to, trigger, actor, detail)| {
                Ok((
                    request_id,
                    RequestEvent {
                        at,
                        from_state: from.as_deref().map(request_state).transpose()?,
                        to_state: request_state(&to)?,
                        trigger: EventTrigger::parse(&trigger)
                            .ok_or_else(|| invalid("trigger", &trigger))?,
                        actor,
                        detail,
                    },
                ))
            });
        let mut by_request: BTreeMap<i64, Vec<RequestEvent>> = BTreeMap::new();
        for (request_id, event) in crate::readable(events)? {
            by_request.entry(request_id).or_default().push(event);
        }
        Ok(by_request)
    }

    /// Acquisitions, newest first; one with `id`, or all in `state`.
    pub async fn list_acquisitions(
        &self,
        id: Option<i64>,
        state: Option<AcquisitionState>,
    ) -> Result<Vec<AcquisitionRecord>, DbError> {
        let rows: Vec<AcquisitionRow> = sqlx::query_as(concat!(
            "SELECT a.id, a.catalog_item_id, coalesce(p.id, c.id) AS title_item_id,
                    coalesce(p.title, c.title) AS title, coalesce(p.year, c.year) AS year,
                    coalesce(p.poster_path, c.poster_path) AS poster_path, c.season_number,
                    c.kind AS unit_kind, c.title AS unit_title, c.year AS unit_year,
                    c.poster_path AS unit_poster, ",
            mb_release_group_key!("c.id"),
            " AS release_group,
                    a.category, a.service_instance_id, s.name AS service_name, s.adapter_type,
                    a.state, a.progress, a.download_stage, a.source_message, a.error_code,
                    a.last_error, ",
            media_servers!("a.category"),
            " AS library_name,
                    a.submit_attempts, a.next_attempt_at, a.submitted_at, a.available_at,
                    a.download_stage_since, a.synced_at, a.created_at, a.updated_at,
                    (SELECT count(*) FROM request_item i
                     WHERE i.acquisition_id = a.id AND i.decision = 'approved') AS requests,
                    s.writes_paused_at IS NOT NULL AS writes_paused, a.stop_requested_at,
                    a.stopped_at
             FROM acquisition a
             JOIN catalog_item c ON c.id = a.catalog_item_id
             LEFT JOIN catalog_item p ON p.id = c.parent_id
             LEFT JOIN service_instance s ON s.id = a.service_instance_id
             WHERE (?1 IS NULL OR a.id = ?1) AND (?2 IS NULL OR a.state = ?2)
             ORDER BY a.created_at DESC, a.id DESC LIMIT 500"
        ))
        .bind(id)
        .bind(state.map(AcquisitionState::as_str))
        .fetch_all(self.pool())
        .await?;
        crate::readable(rows.into_iter().map(TryInto::try_into))
    }

    /// Start a failed acquisition over: pending, fresh attempts, due now.
    /// One whose source was removed moves to `source` (the category's
    /// current source); if that source has an acquisition of the unit
    /// already, the items move there instead.
    #[allow(clippy::single_match_else)] // the three cases read clearer as matches
    pub async fn retry_acquisition(
        &self,
        id: i64,
        source: Option<&SourceTarget>,
        actor: Actor<'_>,
        now: DateTime<Utc>,
    ) -> Result<RetryOutcome, DbError> {
        let mut tx = self.begin_write().await?;
        let row: Option<(String, Option<i64>, i64, String)> = sqlx::query_as(
            "SELECT state, service_instance_id, catalog_item_id, category FROM acquisition
             WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((state, instance, unit, raw_category)) = row else {
            return Ok(RetryOutcome::NotFailed);
        };
        if acquisition_state(&state)? != AcquisitionState::Failed {
            return Ok(RetryOutcome::NotFailed);
        }
        let target = match instance {
            Some(_) => {
                sqlx::query(
                    "UPDATE acquisition SET state = 'pending', submit_attempts = 0,
                         next_attempt_at = NULL, error_code = NULL, last_error = NULL,
                         progress = NULL, source_message = NULL, download_stage = NULL, download_stage_since = NULL, updated_at = ?1
                     WHERE id = ?2",
                )
                .bind(ts(now))
                .bind(id)
                .execute(&mut *tx)
                .await?;
                id
            }
            None => {
                let cat = category(&raw_category)?;
                let Some(source) = source.filter(|s| s.category == cat) else {
                    return Ok(RetryOutcome::NoSource);
                };
                match existing_acquisition(&mut tx, unit, source.service_instance_id).await? {
                    Some((other, _)) => {
                        sqlx::query(
                            "UPDATE request_item SET acquisition_id = ? WHERE acquisition_id = ?",
                        )
                        .bind(other)
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                        sqlx::query("DELETE FROM acquisition WHERE id = ?")
                            .bind(id)
                            .execute(&mut *tx)
                            .await?;
                        // Starts the other one over if it failed too.
                        ensure_acquisition(&mut tx, unit, source, now).await?;
                        other
                    }
                    None => {
                        sqlx::query(
                            "UPDATE acquisition SET service_instance_id = ?1,
                                 quality_profile_id = ?2, root_folder_path = ?3,
                                 minimum_availability = ?4, metadata_profile_id = ?7,
                                 state = 'pending',
                                 submit_attempts = 0, next_attempt_at = NULL, error_code = NULL,
                                 last_error = NULL, progress = NULL, source_message = NULL, download_stage = NULL, download_stage_since = NULL,
                                 remote_id = NULL, updated_at = ?5
                             WHERE id = ?6",
                        )
                        .bind(source.service_instance_id)
                        .bind(source.quality_profile_id)
                        .bind(&source.root_folder_path)
                        .bind(source.minimum_availability.map(MinimumAvailability::as_str))
                        .bind(ts(now))
                        .bind(id)
                        .bind(source.metadata_profile_id)
                        .execute(&mut *tx)
                        .await?;
                        id
                    }
                }
            }
        };
        refresh_linked(&mut tx, target, actor, Some("acquisition retried"), now).await?;
        tx.commit().await?;
        Ok(RetryOutcome::Retried(target))
    }

    /// Open requests of `category` whose acquisitions are at service
    /// instance `id` and not finished: they keep finishing there when the
    /// category's source is switched or removed.
    pub async fn open_requests_at(
        &self,
        instance_id: i64,
        category: Category,
    ) -> Result<Vec<i64>, DbError> {
        let rows: Vec<(i64,)> = sqlx::query_as(
            "SELECT DISTINCT i.request_id FROM request_item i
             JOIN acquisition a ON a.id = i.acquisition_id
             WHERE a.service_instance_id = ? AND a.category = ?
               AND a.state NOT IN ('available', 'failed', 'cancelled')
               AND i.decision = 'approved'
             ORDER BY i.request_id",
        )
        .bind(instance_id)
        .bind(category.as_str())
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// What resuming the paused writes of source `instance_id` sends out:
    /// how many acquisitions and stops are held there and the first `limit`
    /// of each, oldest first (the order `requests.submit` takes them).
    /// Nothing while writes go out.
    pub async fn held_at(&self, instance_id: i64, limit: u32) -> Result<Held, DbError> {
        let (total, preview) = self
            .held_titles(instance_id, "a.state = 'pending'", "a.created_at", limit)
            .await?;
        let (stops, stop_preview) = self
            .held_titles(
                instance_id,
                "a.stop_requested_at IS NOT NULL",
                "a.stop_requested_at",
                limit,
            )
            .await?;
        Ok(Held {
            total,
            preview,
            stops,
            stop_preview,
        })
    }

    /// The acquisitions held at `instance_id` that match `condition` (an SQL
    /// literal): how many, and the first `limit` by `order`.
    async fn held_titles(
        &self,
        instance_id: i64,
        condition: &'static str,
        order: &'static str,
        limit: u32,
    ) -> Result<(u32, Vec<HeldTitle>), DbError> {
        let from = format!(
            "FROM acquisition a
             JOIN service_instance s ON s.id = a.service_instance_id
             JOIN catalog_item c ON c.id = a.catalog_item_id
             LEFT JOIN catalog_item p ON p.id = c.parent_id
             WHERE a.service_instance_id = ?1 AND {condition}
               AND s.writes_paused_at IS NOT NULL"
        );
        let (total,): (i64,) =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT count(*) {from}")))
                .bind(instance_id)
                .fetch_one(self.pool())
                .await?;
        let rows: Vec<HeldRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT coalesce(p.title, c.title), coalesce(p.year, c.year), c.season_number,
                        iif(c.kind = 'album', c.title, NULL), a.category
                 {from} ORDER BY {order}, a.id LIMIT ?2"
        )))
        .bind(instance_id)
        .bind(i64::from(limit))
        .fetch_all(self.pool())
        .await?;
        let preview = rows
            .into_iter()
            .map(|(title, year, season_number, album, raw)| {
                Ok(HeldTitle {
                    title,
                    year,
                    season: season(season_number),
                    album,
                    category: category(&raw)?,
                })
            })
            .collect::<Result<_, DbError>>()?;
        Ok((count(total), preview))
    }

    /// Pending acquisitions due for submission at `now`, oldest first;
    /// held ones (their source's writes are paused) are not due.
    pub async fn due_submissions(&self, now: DateTime<Utc>) -> Result<Vec<SubmitUnit>, DbError> {
        let rows: Vec<SubmitRow> = sqlx::query_as(concat!(
            unit_select!(
                "a.service_instance_id, a.quality_profile_id, a.root_folder_path,
                 a.minimum_availability, a.metadata_profile_id, a.submit_attempts, a.updated_at,
                 a.category"
            ),
            " JOIN service_instance s ON s.id = a.service_instance_id
              WHERE a.state = 'pending' AND s.writes_paused_at IS NULL
                AND (a.next_attempt_at IS NULL OR a.next_attempt_at <= ?)
              ORDER BY a.created_at, a.id"
        ))
        .bind(ts(now))
        .fetch_all(self.pool())
        .await?;
        crate::readable(rows.into_iter().map(|r| {
            Ok(SubmitUnit {
                acquisition_id: r.acquisition_id,
                title_item_id: r.title_item_id,
                title: r.title,
                unit_title: r.unit_title,
                season_number: season(r.season_number),
                tmdb_id: unsigned(r.tmdb_id),
                tvdb_id: unsigned(r.tvdb_id),
                mb_artist: mbid(r.mb_artist.as_deref()),
                release_group: mbid(r.release_group.as_deref()),
                stash_scene: mbid(r.stash_scene.as_deref()),
                service_instance_id: r.service_instance_id,
                quality_profile_id: r.quality_profile_id,
                root_folder_path: r.root_folder_path,
                minimum_availability: minimum_availability(r.minimum_availability.as_deref())?,
                metadata_profile_id: r.metadata_profile_id,
                attempts: count(r.submit_attempts),
                updated_at: r.updated_at,
                category: category(&r.category)?,
            })
        }))
    }

    /// Whether any acquisition or stop is due for submission at `now`
    /// (held ones are not).
    pub async fn has_due_submissions(&self, now: DateTime<Utc>) -> Result<bool, DbError> {
        self.has_due_submissions_outside(now, None).await
    }

    /// [`Self::has_due_submissions`] leaving out category `except` (the
    /// private one while it is switched off: its writes are held).
    pub async fn has_due_submissions_outside(
        &self,
        now: DateTime<Utc>,
        except: Option<Category>,
    ) -> Result<bool, DbError> {
        let (due,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM acquisition a
                 JOIN service_instance s ON s.id = a.service_instance_id
                 WHERE (a.state = 'pending' OR a.stop_requested_at IS NOT NULL)
                   AND s.writes_paused_at IS NULL
                   AND (a.next_attempt_at IS NULL OR a.next_attempt_at <= ?1)
                   AND (?2 IS NULL OR a.category <> ?2))",
        )
        .bind(ts(now))
        .bind(except.map(Category::as_str))
        .fetch_one(self.pool())
        .await?;
        Ok(due)
    }

    /// Stops due at `now`, oldest first; held ones (their source's writes
    /// are paused) are not due.
    pub async fn due_stops(&self, now: DateTime<Utc>) -> Result<Vec<StopUnit>, DbError> {
        let rows: Vec<SubmitRow> = sqlx::query_as(concat!(
            unit_select!(
                "a.service_instance_id, a.quality_profile_id, a.root_folder_path,
                 a.minimum_availability, a.metadata_profile_id, a.submit_attempts, a.updated_at,
                 a.category"
            ),
            " JOIN service_instance s ON s.id = a.service_instance_id
                  WHERE a.stop_requested_at IS NOT NULL AND s.writes_paused_at IS NULL
                    AND (a.next_attempt_at IS NULL OR a.next_attempt_at <= ?)
                  ORDER BY a.stop_requested_at, a.id"
        ))
        .bind(ts(now))
        .fetch_all(self.pool())
        .await?;
        crate::readable(rows.into_iter().map(|r| {
            Ok(StopUnit {
                acquisition_id: r.acquisition_id,
                title_item_id: r.title_item_id,
                title: r.title,
                unit_title: r.unit_title,
                season_number: season(r.season_number),
                tmdb_id: unsigned(r.tmdb_id),
                tvdb_id: unsigned(r.tvdb_id),
                mb_artist: mbid(r.mb_artist.as_deref()),
                release_group: mbid(r.release_group.as_deref()),
                stash_scene: mbid(r.stash_scene.as_deref()),
                service_instance_id: r.service_instance_id,
                category: category(&r.category)?,
            })
        }))
    }

    /// Whether the stop of acquisition `id` is still due, checked right
    /// before the write: an approval may have taken it back, or its files
    /// arrived meanwhile (then the stop is dropped: files present means
    /// nothing in the source, spec 0004 R1).
    pub async fn stop_pending(&self, id: i64, now: DateTime<Utc>) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let still = keep_stop_if_due(&mut tx, id, now).await?;
        tx.commit().await?;
        Ok(still)
    }

    /// The source stopped monitoring what Starr turned on for acquisition
    /// `id` (read back), or had nothing left to stop. A searching one is
    /// cancelled; a downloading one stays followed until its file lands or
    /// its download goes away. Wanted again meanwhile (an approval raced
    /// the write), it starts over as pending so its monitoring comes back.
    /// Returns whether it waits for submission again.
    pub async fn record_stopped(&self, id: i64, now: DateTime<Utc>) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let row: Option<(String,)> = sqlx::query_as("SELECT state FROM acquisition WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some((raw,)) = row else {
            return Ok(false);
        };
        let state = acquisition_state(&raw)?;
        let restart = wanted(&mut tx, id).await? && state != AcquisitionState::Available;
        let next = if restart {
            AcquisitionState::Pending
        } else if state == AcquisitionState::Submitted {
            AcquisitionState::Cancelled
        } else {
            state
        };
        sqlx::query(
            "UPDATE acquisition SET state = ?1, stop_requested_at = NULL, next_attempt_at = NULL,
                 stopped_at = iif(?2, NULL, ?3), starr_added = 0, starr_monitored = 0,
                 error_code = iif(?1 = 'failed', error_code, NULL),
                 last_error = iif(?1 = 'failed', last_error, NULL),
                 progress = iif(?1 = state, progress, NULL),
                 source_message = iif(?1 = state, source_message, NULL),
                 download_stage = iif(?1 = state, download_stage, NULL),
                 download_stage_since = iif(?1 = state, download_stage_since, NULL),
                 submit_attempts = iif(?2, 0, submit_attempts), updated_at = ?3
             WHERE id = ?4",
        )
        .bind(next.as_str())
        .bind(restart)
        .bind(ts(now))
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if restart {
            refresh_linked(
                &mut tx,
                id,
                RESTART,
                Some("monitoring switched on again"),
                now,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(restart)
    }

    /// Stopping acquisition `id` failed with taxonomy code `code` and
    /// readable `reason`: due again at `retry_at`, or given up without one
    /// (the reason stays readable for admins; the title stays monitored).
    pub async fn record_stop_failure(
        &self,
        id: i64,
        code: &str,
        reason: &str,
        retry_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE acquisition SET next_attempt_at = ?1, error_code = ?2, last_error = ?3,
                 stop_requested_at = iif(?1 IS NULL, NULL, stop_requested_at), updated_at = ?4
             WHERE id = ?5 AND stop_requested_at IS NOT NULL",
        )
        .bind(retry_at.map(ts))
        .bind(code)
        .bind(reason)
        .bind(ts(now))
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The acquisitions `acquisitions.sync` follows, grouped by title.
    pub async fn synced_acquisitions(&self) -> Result<Vec<SyncUnit>, DbError> {
        let rows: Vec<SyncRow> = sqlx::query_as(concat!(
            unit_select!(
                "a.category, a.service_instance_id, a.state, a.download_stage,
                 a.download_stage_since"
            ),
            " WHERE a.state IN ('submitted', 'downloading', 'partially_available')
                AND a.service_instance_id IS NOT NULL
              ORDER BY a.service_instance_id, title_item_id, c.season_number, a.id"
        ))
        .fetch_all(self.pool())
        .await?;
        crate::readable(rows.into_iter().map(|r| {
            let state = acquisition_state(&r.state)?;
            let arriving =
                download_stage(state, r.download_stage.as_deref()) == Some(DownloadStage::Arriving);
            Ok(SyncUnit {
                arriving_since: r.download_stage_since.filter(|_| arriving),
                acquisition_id: r.acquisition_id,
                title_item_id: r.title_item_id,
                title: r.title,
                unit_title: r.unit_title,
                season_number: season(r.season_number),
                tmdb_id: unsigned(r.tmdb_id),
                tvdb_id: unsigned(r.tvdb_id),
                imdb_id: r.imdb_id,
                mb_artist: mbid(r.mb_artist.as_deref()),
                release_group: mbid(r.release_group.as_deref()),
                stash_scene: mbid(r.stash_scene.as_deref()),
                category: category(&r.category)?,
                service_instance_id: r.service_instance_id,
                state,
            })
        }))
    }

    /// Whether `acquisitions.sync` has anything to follow.
    pub async fn has_synced_acquisitions(&self) -> Result<bool, DbError> {
        let (any,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM acquisition
                 WHERE state IN ('submitted', 'downloading', 'partially_available'))",
        )
        .fetch_one(self.pool())
        .await?;
        Ok(any)
    }

    /// The source took pending acquisition `id` (added or monitored, read
    /// back); `changed` is what Starr changed there, for a later stop.
    /// `false` if it was no longer pending.
    pub async fn record_submitted(
        &self,
        id: i64,
        remote_id: Option<&str>,
        changed: SourceChange,
        actor: Actor<'_>,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let res = sqlx::query(
            "UPDATE acquisition SET state = 'submitted', submitted_at = ?1, updated_at = ?1,
                 submit_attempts = submit_attempts + 1, next_attempt_at = NULL,
                 error_code = NULL, last_error = NULL, remote_id = coalesce(?2, remote_id),
                 starr_added = ?4, starr_monitored = ?5
             WHERE id = ?3 AND state = 'pending'",
        )
        .bind(ts(now))
        .bind(remote_id)
        .bind(id)
        .bind(changed.added)
        .bind(changed.added || changed.monitored)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            // Cancelled while the write was on its way (spec 0004 R1): what
            // Starr turned on must not be left behind unknown. It is
            // searching now, so releasing it again queues the stop.
            let late = sqlx::query(
                "UPDATE acquisition SET state = 'submitted', submitted_at = ?1, updated_at = ?1,
                     next_attempt_at = NULL, remote_id = coalesce(?2, remote_id),
                     starr_added = ?4, starr_monitored = ?5
                 WHERE id = ?3 AND state = 'cancelled' AND ?5",
            )
            .bind(ts(now))
            .bind(remote_id)
            .bind(id)
            .bind(changed.added)
            .bind(changed.added || changed.monitored)
            .execute(&mut *tx)
            .await?;
            if late.rows_affected() > 0 {
                release_acquisition(&mut tx, id, now).await?;
                tx.commit().await?;
            }
            return Ok(false);
        }
        refresh_linked(&mut tx, id, actor, None, now).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Whether acquisition `id` is still pending (not cancelled meanwhile):
    /// checked right before the write.
    pub async fn submission_pending(&self, id: i64) -> Result<bool, DbError> {
        let (pending,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM acquisition WHERE id = ? AND state = 'pending')",
        )
        .bind(id)
        .fetch_one(self.pool())
        .await?;
        Ok(pending)
    }

    /// A submission of pending acquisition `id` failed with taxonomy code
    /// `code` and readable `reason`: due again at `retry_at`, or failed for
    /// good without one. `false` if it was no longer pending.
    pub async fn record_submit_failure(
        &self,
        id: i64,
        code: &str,
        reason: &str,
        retry_at: Option<DateTime<Utc>>,
        actor: Actor<'_>,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let state = if retry_at.is_some() {
            AcquisitionState::Pending
        } else {
            AcquisitionState::Failed
        };
        let res = sqlx::query(
            "UPDATE acquisition SET state = ?1, submit_attempts = submit_attempts + 1,
                 next_attempt_at = ?2, error_code = ?3, last_error = ?4, updated_at = ?5
             WHERE id = ?6 AND state = 'pending'",
        )
        .bind(state.as_str())
        .bind(retry_at.map(ts))
        .bind(code)
        .bind(reason)
        .bind(ts(now))
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            return Ok(false);
        }
        if state == AcquisitionState::Failed {
            notifications::acquisition_failed(&mut tx, id, reason, now).await?;
        }
        refresh_linked(&mut tx, id, actor, Some(reason), now).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Store what `acquisitions.sync` observed for acquisition `id` (state,
    /// progress, message), unless the state change is not an allowed
    /// transition: then nothing is written. One nobody wants any more whose
    /// download went away without a file is cancelled instead of searching
    /// again (spec 0004 R1; a pending stop goes out first). Returns the
    /// state change, if any.
    pub async fn record_observed(
        &self,
        id: i64,
        observed: &Observed,
        actor: Actor<'_>,
        now: DateTime<Utc>,
    ) -> Result<Option<(AcquisitionState, AcquisitionState)>, DbError> {
        let mut tx = self.begin_write().await?;
        let row: Option<(String, bool)> = sqlx::query_as(
            "SELECT state, stop_requested_at IS NOT NULL FROM acquisition WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((raw, stop_pending)) = row else {
            return Ok(None);
        };
        let before = acquisition_state(&raw)?;
        if before != observed.state && !before.can_move_to(observed.state) {
            return Ok(None);
        }
        if observed.state == AcquisitionState::Submitted
            && !stop_pending
            && !wanted(&mut tx, id).await?
        {
            cancel_acquisition(&mut tx, id, now).await?;
            sqlx::query("UPDATE acquisition SET synced_at = ? WHERE id = ?")
                .bind(ts(now))
                .bind(id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok(Some((before, AcquisitionState::Cancelled)));
        }
        sqlx::query(
            "UPDATE acquisition SET state = ?1, progress = ?2, source_message = ?3, synced_at = ?4,
                 download_stage_since = CASE WHEN ?6 IS NULL THEN NULL
                     WHEN download_stage IS ?6 AND state = 'downloading'
                         THEN coalesce(download_stage_since, ?4)
                     ELSE ?4 END,
                 download_stage = ?6,
                 available_at = iif(?1 = 'available', coalesce(available_at, ?4), available_at),
                 updated_at = iif(state = ?1, updated_at, ?4)
             WHERE id = ?5",
        )
        .bind(observed.state.as_str())
        .bind(observed.progress)
        .bind(observed.message.as_deref())
        .bind(ts(now))
        .bind(id)
        .bind(observed.stage.map(DownloadStage::as_str))
        .execute(&mut *tx)
        .await?;
        if stop_pending {
            keep_stop_if_due(&mut tx, id, now).await?;
        }
        let change = (before != observed.state).then_some((before, observed.state));
        if change.is_some() {
            refresh_linked(&mut tx, id, actor, None, now).await?;
        }
        if let Some(message) = observed
            .message
            .as_deref()
            .filter(|_| observed.import_blocked())
        {
            notifications::import_blocked(&mut tx, id, message, now).await?;
        }
        tx.commit().await?;
        Ok(change)
    }

    /// A followed acquisition failed for good (the title left the source):
    /// failed with `reason`, or cancelled without telling anyone if nobody
    /// wants it any more. `false` if it was not followed.
    pub async fn fail_acquisition(
        &self,
        id: i64,
        code: &str,
        reason: &str,
        actor: Actor<'_>,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        if !wanted(&mut tx, id).await? {
            let res = sqlx::query(
                "UPDATE acquisition SET synced_at = ?1 WHERE id = ?2
                     AND state IN ('submitted', 'downloading', 'partially_available')",
            )
            .bind(ts(now))
            .bind(id)
            .execute(&mut *tx)
            .await?;
            if res.rows_affected() > 0 {
                cancel_acquisition(&mut tx, id, now).await?;
            }
            tx.commit().await?;
            return Ok(res.rows_affected() > 0);
        }
        let res = sqlx::query(
            "UPDATE acquisition SET state = 'failed', error_code = ?1, last_error = ?2,
                 updated_at = ?3, synced_at = ?3
             WHERE id = ?4 AND state IN ('submitted', 'downloading', 'partially_available')",
        )
        .bind(code)
        .bind(reason)
        .bind(ts(now))
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            return Ok(false);
        }
        notifications::acquisition_failed(&mut tx, id, reason, now).await?;
        refresh_linked(&mut tx, id, actor, Some(reason), now).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Available acquisitions whose unit left the library: the category's
    /// consume services list the title only as gone (after two complete
    /// runs and the minimum time, the inventory's rule) and list it nowhere
    /// as present. `acquisitions.sync` asks the source for files before it
    /// calls them removed ([`Self::record_removed`], spec 0004 R4). A
    /// season counts as gone with its series. One the source still had
    /// files for is looked at again only [`REMOVAL_RECHECK`] after that
    /// check ([`Self::record_removal_kept`]).
    pub async fn removal_candidates(&self, now: DateTime<Utc>) -> Result<Vec<SyncUnit>, DbError> {
        let rows: Vec<SyncRow> = sqlx::query_as(concat!(
            "SELECT * FROM (",
            unit_select!(
                "a.category, a.service_instance_id, a.state, a.download_stage,
                 a.download_stage_since"
            ),
            " WHERE a.state = 'available' AND a.service_instance_id IS NOT NULL
                AND (a.synced_at IS NULL OR a.synced_at <= ?1)) u
              WHERE EXISTS (",
            removal_inventory!("i.gone_since IS NOT NULL"),
            ")
                AND NOT EXISTS (",
            removal_inventory!("i.present_since IS NOT NULL"),
            ")
              ORDER BY u.service_instance_id, u.title_item_id, u.season_number,
                       u.acquisition_id"
        ))
        .bind(ts(now - REMOVAL_RECHECK))
        .fetch_all(self.pool())
        .await?;
        crate::readable(rows.into_iter().map(|r| {
            Ok(SyncUnit {
                arriving_since: None,
                acquisition_id: r.acquisition_id,
                title_item_id: r.title_item_id,
                title: r.title,
                unit_title: r.unit_title,
                season_number: season(r.season_number),
                tmdb_id: unsigned(r.tmdb_id),
                tvdb_id: unsigned(r.tvdb_id),
                imdb_id: r.imdb_id,
                mb_artist: mbid(r.mb_artist.as_deref()),
                release_group: mbid(r.release_group.as_deref()),
                stash_scene: mbid(r.stash_scene.as_deref()),
                category: category(&r.category)?,
                service_instance_id: r.service_instance_id,
                state: acquisition_state(&r.state)?,
            })
        }))
    }

    /// Whether [`Self::removal_candidates`] has any at `now`.
    pub async fn has_removal_candidates(&self, now: DateTime<Utc>) -> Result<bool, DbError> {
        Ok(!self.removal_candidates(now).await?.is_empty())
    }

    /// The source still has files for available acquisition `id` although
    /// the consume services list it as gone: kept, and looked at again
    /// after [`REMOVAL_RECHECK`].
    pub async fn record_removal_kept(&self, id: i64, now: DateTime<Utc>) -> Result<(), DbError> {
        sqlx::query("UPDATE acquisition SET synced_at = ? WHERE id = ? AND state = 'available'")
            .bind(ts(now))
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Available acquisition `id` left the library (gone from the consume
    /// services, no file in the source): it is cancelled and the approved
    /// items linked to it end as removed, silently (spec 0004 R4). The
    /// unit can be requested again. `false` if it was not available.
    pub async fn record_removed(
        &self,
        id: i64,
        actor: Actor<'_>,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let removed = remove_available(&mut tx, id, actor, now).await?;
        tx.commit().await?;
        Ok(removed)
    }

    /// Available scenes whose file `acquisitions.sync` asks Whisparr about
    /// again (spec 0007 Q5 A): adult availability comes from Whisparr's
    /// files only, so "removed" (spec 0004 R4) is decided there. Each is
    /// looked at again [`REMOVAL_RECHECK`] after its last check.
    pub async fn scene_file_checks(&self, now: DateTime<Utc>) -> Result<Vec<SyncUnit>, DbError> {
        let rows: Vec<SyncRow> = sqlx::query_as(concat!(
            unit_select!(
                "a.category, a.service_instance_id, a.state, a.download_stage,
                 a.download_stage_since"
            ),
            " WHERE a.state = 'available' AND c.kind = 'scene'
                AND a.service_instance_id IS NOT NULL
                AND (a.synced_at IS NULL OR a.synced_at <= ?1)
              ORDER BY a.service_instance_id, title_item_id, a.id"
        ))
        .bind(ts(now - REMOVAL_RECHECK))
        .fetch_all(self.pool())
        .await?;
        crate::readable(rows.into_iter().map(|r| {
            Ok(SyncUnit {
                arriving_since: None,
                acquisition_id: r.acquisition_id,
                title_item_id: r.title_item_id,
                title: r.title,
                unit_title: r.unit_title,
                season_number: season(r.season_number),
                tmdb_id: unsigned(r.tmdb_id),
                tvdb_id: unsigned(r.tvdb_id),
                imdb_id: r.imdb_id,
                mb_artist: mbid(r.mb_artist.as_deref()),
                release_group: mbid(r.release_group.as_deref()),
                stash_scene: mbid(r.stash_scene.as_deref()),
                category: category(&r.category)?,
                service_instance_id: r.service_instance_id,
                state: acquisition_state(&r.state)?,
            })
        }))
    }

    /// Whether [`Self::scene_file_checks`] has any at `now`.
    pub async fn has_scene_file_checks(&self, now: DateTime<Utc>) -> Result<bool, DbError> {
        let (any,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM acquisition a
                 JOIN catalog_item c ON c.id = a.catalog_item_id
                 WHERE a.state = 'available' AND c.kind = 'scene'
                   AND a.service_instance_id IS NOT NULL
                   AND (a.synced_at IS NULL OR a.synced_at <= ?))",
        )
        .bind(ts(now - REMOVAL_RECHECK))
        .fetch_one(self.pool())
        .await?;
        Ok(any)
    }

    /// What Whisparr showed of available scene `id` ([`Self::scene_file_checks`]):
    /// its file (kept), no file for the first time (noted), or no file
    /// again on a later check (removed: the approved items end as removed,
    /// silently, and the scene can be requested again, spec 0004 R4).
    pub async fn record_scene_file(
        &self,
        id: i64,
        has_file: bool,
        actor: Actor<'_>,
        now: DateTime<Utc>,
    ) -> Result<SceneFile, DbError> {
        let mut tx = self.begin_write().await?;
        let row: Option<(Option<String>,)> = sqlx::query_as(
            "SELECT file_missing_since FROM acquisition WHERE id = ? AND state = 'available'",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((missing_since,)) = row else {
            return Ok(SceneFile::NotAvailable);
        };
        let outcome = if has_file {
            sqlx::query(
                "UPDATE acquisition SET file_missing_since = NULL, synced_at = ?1 WHERE id = ?2",
            )
            .bind(ts(now))
            .bind(id)
            .execute(&mut *tx)
            .await?;
            SceneFile::Kept
        } else if missing_since.is_none() {
            sqlx::query(
                "UPDATE acquisition SET file_missing_since = ?1, synced_at = ?1 WHERE id = ?2",
            )
            .bind(ts(now))
            .bind(id)
            .execute(&mut *tx)
            .await?;
            SceneFile::Missing
        } else if remove_available(&mut tx, id, actor, now).await? {
            SceneFile::Removed
        } else {
            SceneFile::NotAvailable
        };
        tx.commit().await?;
        Ok(outcome)
    }

    /// Albums the music source has files for that no music consume service
    /// lists by their release group, arriving since `before` or earlier
    /// (spec 0006 Q7: likely files without `MusicBrainz` tags, which Starr
    /// never matches by name). Oldest first.
    pub async fn unlisted_albums(
        &self,
        before: DateTime<Utc>,
    ) -> Result<Vec<UnlistedAlbum>, DbError> {
        type Row = (
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            DateTime<Utc>,
        );
        let rows: Vec<Row> = sqlx::query_as(concat!(
            "SELECT a.id, coalesce(p.title, c.title), c.title, ",
            mb_release_group_key!("c.id"),
            ", s.name, ",
            media_servers!("a.category"),
            ", a.download_stage_since
             FROM acquisition a
             JOIN catalog_item c ON c.id = a.catalog_item_id AND c.kind = 'album'
             LEFT JOIN catalog_item p ON p.id = c.parent_id
             LEFT JOIN service_instance s ON s.id = a.service_instance_id
             WHERE a.category = 'music' AND a.state = 'downloading'
               AND a.download_stage = 'arriving' AND a.download_stage_since IS NOT NULL
               AND a.download_stage_since <= ?1
             ORDER BY a.download_stage_since, a.id"
        ))
        .bind(ts(before))
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(acquisition_id, artist, album, release_group, source, libraries, since)| {
                    UnlistedAlbum {
                        acquisition_id,
                        artist,
                        album,
                        release_group: mbid(release_group.as_deref()),
                        source,
                        libraries,
                        since,
                    }
                },
            )
            .collect())
    }

    /// The consume services of `category` by name, " / " between several
    /// (where an arriving title will show up); `None` without any.
    pub async fn library_name(&self, category: Category) -> Result<Option<String>, DbError> {
        let (name,): (Option<String>,) = sqlx::query_as(concat!("SELECT ", media_servers!("?")))
            .bind(category.as_str())
            .fetch_one(self.pool())
            .await?;
        Ok(name)
    }

    /// What the consume services of `category` have of a title, by any of
    /// its natural keys (inventory, spec 0003 slice 6). Gone rows do not
    /// count.
    #[allow(clippy::similar_names)] // the three natural keys
    pub async fn title_presence(
        &self,
        category: Category,
        kind: ItemKind,
        tmdb_id: Option<u64>,
        tvdb_id: Option<u64>,
        imdb_id: Option<&str>,
    ) -> Result<TitlePresence, DbError> {
        // Albums are asked for by release group.
        if matches!(kind, ItemKind::Artist | ItemKind::Album) {
            return Err(DbError::InvalidData(
                "music presence is read with album_presence".to_owned(),
            ));
        }
        // Scenes are available when Whisparr has their file (spec 0007 Q5
        // A): the adult libraries of the media servers are never asked.
        if matches!(
            kind,
            ItemKind::Scene | ItemKind::Performer | ItemKind::Studio
        ) {
            return Ok(TitlePresence::NoService);
        }
        if !self.watched(category).await? {
            return Ok(TitlePresence::NoService);
        }
        let to_i64 = |v: Option<u64>| v.and_then(|v| i64::try_from(v).ok());
        let rows: Vec<(String,)> = sqlx::query_as(concat!(
            "SELECT i.seasons FROM inventory i
             JOIN service_role r ON r.service_instance_id = i.service_instance_id
                  AND r.kind = 'consume' AND r.category = ?1
             WHERE i.kind = ?2 AND i.present_since IS NOT NULL
               AND ((?3 IS NOT NULL AND i.tmdb_id = ?3) OR (?4 IS NOT NULL AND i.tvdb_id = ?4)
                    OR (?5 IS NOT NULL AND i.imdb_id = ?5))
               AND ",
            in_role_libraries!("r", "i")
        ))
        .bind(category.as_str())
        .bind(kind.as_str())
        .bind(to_i64(tmdb_id))
        .bind(to_i64(tvdb_id))
        .bind(imdb_id)
        .fetch_all(self.pool())
        .await?;
        if rows.is_empty() {
            return Ok(TitlePresence::Absent);
        }
        let mut merged: BTreeMap<u32, u32> = BTreeMap::new();
        for (json,) in rows {
            let seasons: BTreeMap<u32, u32> = serde_json::from_str(&json)?;
            for (number, episodes) in seasons {
                let entry = merged.entry(number).or_default();
                *entry = (*entry).max(episodes);
            }
        }
        Ok(TitlePresence::Present(merged))
    }

    /// Whether `category` watches in any consume service.
    async fn watched(&self, category: Category) -> Result<bool, DbError> {
        let (watched,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM service_role WHERE category = ? AND kind = 'consume')",
        )
        .bind(category.as_str())
        .fetch_one(self.pool())
        .await?;
        Ok(watched)
    }

    /// What the consume services of the music category (Navidrome,
    /// Jellyfin/Emby music libraries) have of these albums, by release
    /// group only (spec 0006 Q7 B: never by name). Without a music consume
    /// service, availability comes from the source's files
    /// ([`TitlePresence::NoService`]). Gone rows do not count.
    pub async fn album_presence(
        &self,
        category: Category,
        release_groups: &[Uuid],
    ) -> Result<TitlePresence, DbError> {
        if !self.watched(category).await? {
            return Ok(TitlePresence::NoService);
        }
        if release_groups.is_empty() {
            return Ok(TitlePresence::Albums(BTreeMap::new()));
        }
        let keys: Vec<String> = release_groups
            .iter()
            .map(|rg| crate::inventory::album_key(*rg))
            .collect();
        let rows: Vec<(String, Option<i64>)> = sqlx::query_as(concat!(
            "SELECT i.natural_key, max(i.tracks) FROM inventory i
             JOIN service_role r ON r.service_instance_id = i.service_instance_id
                  AND r.kind = 'consume' AND r.category = ?1
             WHERE i.kind = 'album' AND i.present_since IS NOT NULL
               AND i.natural_key IN (SELECT value FROM json_each(?2))
               AND ",
            in_role_libraries!("r", "i"),
            " GROUP BY i.natural_key"
        ))
        .bind(category.as_str())
        .bind(serde_json::to_string(&keys)?)
        .fetch_all(self.pool())
        .await?;
        Ok(TitlePresence::Albums(
            rows.into_iter()
                .filter_map(|(key, tracks)| {
                    let rg = crate::inventory::release_group_of(&key)?;
                    let tracks = tracks.and_then(|t| u32::try_from(t).ok()).unwrap_or(0);
                    Some((rg, tracks))
                })
                .collect(),
        ))
    }

    /// The units of title `title_id` (the movie or scene itself, its seasons
    /// by number, or the albums of an artist that Starr stored) with what is
    /// known about each, as seen by `member_id`.
    pub async fn unit_facts(
        &self,
        title_id: i64,
        member_id: i64,
    ) -> Result<Vec<UnitFacts>, DbError> {
        let rows: Vec<FactsRow> = sqlx::query_as(concat!(
            "SELECT c.id, c.season_number, ",
            mb_release_group_key!("c.id"),
            ", c.metadata,
                        (SELECT a.state FROM acquisition a WHERE a.catalog_item_id = c.id
                         ORDER BY a.state = 'cancelled', a.state = 'failed', a.id DESC LIMIT 1),
                        (SELECT a.download_stage FROM acquisition a WHERE a.catalog_item_id = c.id
                         ORDER BY a.state = 'cancelled', a.state = 'failed', a.id DESC LIMIT 1),
                        EXISTS (SELECT 1 FROM request_item i WHERE i.catalog_item_id = c.id
                                AND i.decision IN ('pending_approval', 'approved')),
                        EXISTS (SELECT 1 FROM request_item i WHERE i.catalog_item_id = c.id
                                AND i.member_id = ?2
                                AND i.decision IN ('pending_approval', 'approved'))
                 FROM catalog_item c
                 WHERE (c.id = ?1 AND c.kind IN ('movie', 'scene'))
                    OR (c.parent_id = ?1 AND c.kind IN ('season', 'album'))
                 ORDER BY c.season_number, c.id"
        ))
        .bind(title_id)
        .bind(member_id)
        .fetch_all(self.pool())
        .await?;
        crate::readable(rows.into_iter().map(
            |(id, number, release_group, metadata, acquisition, stage, open, mine)| {
                // Seasons: the episodes; albums: the tracks of the release
                // the catalog counts them by.
                let count = if number.is_some() {
                    Some("episodeCount")
                } else if release_group.is_some() {
                    Some("trackCount")
                } else {
                    None
                };
                let expected_episodes = match (count, metadata.as_deref()) {
                    (Some(field), Some(json)) => serde_json::from_str::<serde_json::Value>(json)?
                        .get(field)
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|n| u32::try_from(n).ok()),
                    _ => None,
                };
                let acquisition = acquisition.as_deref().map(acquisition_state).transpose()?;
                let stage = match acquisition {
                    Some(state) => download_stage(state, stage.as_deref()),
                    None => None,
                };
                Ok(UnitFacts {
                    catalog_item_id: id,
                    season_number: season(number),
                    release_group: mbid(release_group.as_deref()),
                    expected_episodes,
                    acquisition,
                    stage,
                    open_requests: open,
                    mine,
                })
            },
        ))
    }
}

#[cfg(test)]
#[path = "requests_tests.rs"]
mod tests;
