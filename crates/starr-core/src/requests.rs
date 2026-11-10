//! Requests and acquisitions (domain model §6, spec 0003 D6, D8, D10, D11,
//! D15). Pure rules; storage lives in `starr-db`, submission and sync in
//! `starr-worker`.
//!
//! A **request** is personal: one member asks for one title. Its sub-items
//! are the units that can be acquired: the movie itself, or single seasons
//! of a series (D8). Each item carries the admin's (or the request mode's)
//! [`Decision`]. An approved item is linked to the **acquisition** of its
//! unit at the category's source, which is shared by every member asking
//! for the same unit (D10). What a member sees, [`RequestState`], is derived
//! from both through exactly one function per level (lesson 16):
//! [`item_state`] for an item and [`request_state`] for the request.

use std::time::Duration;

use chrono::TimeDelta;

use crate::adapters::AdapterError;
use crate::members::RequestMode;
use crate::source::{FileState, QueueEntry, QueueState};

/// Longest reject reason a member gets to read, in characters.
pub const REJECT_REASON_MAX_CHARS: usize = 500;

macro_rules! names {
    ($ty:ident { $($variant:ident => $name:literal),+ $(,)? }) => {
        impl $ty {
            pub const ALL: [Self; [$(stringify!($variant)),+].len()] = [$(Self::$variant),+];

            /// Stable name used in the database and the API.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }

            #[must_use]
            pub fn parse(raw: &str) -> Option<Self> {
                Self::ALL.into_iter().find(|v| v.as_str() == raw)
            }
        }
    };
}

/// What was decided about one request item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Decision {
    /// Waiting for an admin (request mode `approval`, D6).
    PendingApproval,
    /// Approved by an admin, by request mode `auto`, or by attaching to a
    /// running acquisition (D10).
    Approved,
    /// Rejected by an admin, with a reason the member sees. Final: the item
    /// stays rejected even if the title becomes available another way.
    Rejected,
    /// Withdrawn by the member before a decision (D11), or cancelled by
    /// the member after approval while the download had not started
    /// (spec 0004).
    Cancelled,
    /// Revoked by an admin, with a reason the member sees (spec 0004 R3).
    /// The next request for the unit always waits for approval.
    Revoked,
    /// The unit left the library after it was there: gone from the consume
    /// services' inventory and without a file in the source (spec 0004
    /// R4). The unit can be requested again.
    Removed,
}

names!(Decision {
    PendingApproval => "pending_approval",
    Approved => "approved",
    Rejected => "rejected",
    Cancelled => "cancelled",
    Revoked => "revoked",
    Removed => "removed",
});

impl Decision {
    /// Whether the item still asks for its unit (pending or approved): an
    /// approved one keeps its acquisition wanted.
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::PendingApproval | Self::Approved)
    }
}

/// What someone does to a pending item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionAction {
    /// Admin approves.
    Approve,
    /// Admin rejects (with a reason).
    Reject,
    /// The member withdraws (D11: only until approved).
    Withdraw,
    /// The member cancels an approved item whose download has not started
    /// ([`member_end`], spec 0004 R2).
    Cancel,
    /// An admin revokes a pending or approved item (spec 0004 R3).
    Revoke,
    /// The system ends an approved item whose unit left the library (spec
    /// 0004 R4).
    Remove,
    /// An admin takes a fresh approval back while nothing went to the
    /// source for it (spec 0008 F-4): the item waits again. A rejection is
    /// never undone (D11).
    UndoApprove,
}

/// A decision that cannot be taken from the item's current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("cannot {action:?} an item that is {from:?}")]
pub struct TransitionError {
    pub from: Decision,
    pub action: DecisionAction,
}

/// The decision state machine: a pending item is decided once; an
/// approved one can still be cancelled, revoked or removed (spec 0004);
/// every other decision is final.
pub const fn decide(from: Decision, action: DecisionAction) -> Result<Decision, TransitionError> {
    match (from, action) {
        (Decision::PendingApproval, DecisionAction::Approve) => Ok(Decision::Approved),
        (Decision::PendingApproval, DecisionAction::Reject) => Ok(Decision::Rejected),
        (Decision::PendingApproval, DecisionAction::Withdraw)
        | (Decision::Approved, DecisionAction::Cancel) => Ok(Decision::Cancelled),
        (Decision::PendingApproval | Decision::Approved, DecisionAction::Revoke) => {
            Ok(Decision::Revoked)
        }
        (Decision::Approved, DecisionAction::Remove) => Ok(Decision::Removed),
        (Decision::Approved, DecisionAction::UndoApprove) => Ok(Decision::PendingApproval),
        (from, action) => Err(TransitionError { from, action }),
    }
}

/// How the member ends an item of their own (spec 0004 R2): withdraw while
/// it waits for approval, cancel once approved as long as the download has
/// not started (the acquisition is pending, held, failed, or searching
/// without queue entry and file; `None`: no acquisition yet). `None` when
/// the member cannot end it (downloading, available, or ended already).
#[must_use]
pub const fn member_end(
    decision: Decision,
    acquisition: Option<AcquisitionState>,
) -> Option<DecisionAction> {
    match (decision, acquisition) {
        (Decision::PendingApproval, _) => Some(DecisionAction::Withdraw),
        (
            Decision::Approved,
            None
            | Some(
                AcquisitionState::Pending
                | AcquisitionState::Failed
                | AcquisitionState::Submitted
                | AcquisitionState::Cancelled,
            ),
        ) => Some(DecisionAction::Cancel),
        _ => None,
    }
}

/// State of the shared acquisition of one unit (movie or season) at one
/// source instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AcquisitionState {
    /// Approved, not yet handed to the source (`requests.submit`).
    Pending,
    /// Added to or monitored in the source; nothing downloading yet.
    Submitted,
    /// In the source's download queue, or downloaded and waiting to show
    /// up in a consume service.
    Downloading,
    /// Season: some aired episodes are there (D15).
    PartiallyAvailable,
    /// Watchable: the movie, or every aired episode of the season (D15).
    Available,
    /// Submission failed for good; the reason is stored with it.
    Failed,
    /// Nobody wants it any more (spec 0004 R1): nothing is sent for it and
    /// sync no longer follows it. A new approval starts it over.
    Cancelled,
}

names!(AcquisitionState {
    Pending => "pending",
    Submitted => "submitted",
    Downloading => "downloading",
    PartiallyAvailable => "partially_available",
    Available => "available",
    Failed => "failed",
    Cancelled => "cancelled",
});

impl AcquisitionState {
    /// Whether `acquisitions.sync` follows it (handed to the source, not
    /// complete yet).
    #[must_use]
    pub const fn is_synced(self) -> bool {
        matches!(
            self,
            Self::Submitted | Self::Downloading | Self::PartiallyAvailable
        )
    }

    /// Whether it is held: pending at a source instance whose writes an
    /// admin paused (maintainer, 2026-11-02). `requests.submit` leaves it
    /// alone until the writes are resumed; it is no state of its own, so
    /// pausing and resuming never move an acquisition.
    #[must_use]
    pub const fn is_held(self, writes_paused: bool) -> bool {
        writes_paused && matches!(self, Self::Pending)
    }

    /// Whether moving from `self` to `to` is allowed. Submission moves a
    /// pending acquisition on; sync moves between the submitted states and
    /// to available; a failed or cancelled one starts over as pending
    /// (retry, or a new approval). Any but an available one is cancelled
    /// when nobody wants it any more ([`Self::when_unwanted`]); an
    /// available one only when its unit left the library (spec 0004 R4).
    #[must_use]
    pub const fn can_move_to(self, to: Self) -> bool {
        match self {
            Self::Pending => matches!(to, Self::Submitted | Self::Failed | Self::Cancelled),
            Self::Submitted | Self::Downloading | Self::PartiallyAvailable => matches!(
                to,
                Self::Submitted
                    | Self::Downloading
                    | Self::PartiallyAvailable
                    | Self::Available
                    | Self::Failed
                    | Self::Cancelled
            ),
            Self::Available => matches!(to, Self::Cancelled),
            Self::Failed => matches!(to, Self::Pending | Self::Cancelled),
            Self::Cancelled => matches!(to, Self::Pending),
        }
    }

    /// What happens to the acquisition when the last open request item
    /// linked to it ends (spec 0004 R1). `stage` is its stored download
    /// stage, `starr_monitored` whether Starr itself added the title or
    /// switched its monitoring on (unknown counts as not: D14).
    #[must_use]
    pub const fn when_unwanted(
        self,
        stage: Option<DownloadStage>,
        starr_monitored: bool,
    ) -> Unwanted {
        match (self, DownloadStage::of(self, stage)) {
            // Searching, downloading or importing with the monitoring Starr
            // switched on: turn it off (no more searching); a running
            // download is left alone.
            (Self::Submitted, _) | (Self::Downloading, None | Some(DownloadStage::Importing))
                if starr_monitored =>
            {
                Unwanted::Stop
            }
            // Not sent yet (pending, held), never taken, or searching with
            // monitoring that was there before Starr: nothing goes out.
            (Self::Pending | Self::Failed | Self::Submitted, _) => Unwanted::Cancel,
            // Files present (arriving, partly or fully available), already
            // cancelled, or monitored before Starr: nothing in the source.
            (
                Self::Downloading | Self::PartiallyAvailable | Self::Available | Self::Cancelled,
                _,
            ) => Unwanted::Keep,
        }
    }
}

/// What [`AcquisitionState::when_unwanted`] does with an acquisition that
/// nobody wants any more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unwanted {
    /// Cancel it; nothing is written to the source.
    Cancel,
    /// Queue a stop: the source unmonitors exactly what Starr turned on.
    Stop,
    /// Leave it as it is (files are there, or the download runs on).
    Keep,
}

/// How far a downloading acquisition got once nothing is left to load
/// (maintainer's acceptance test, 2026-11-02): "Downloading 100%" for a
/// title the source imported long ago reads as stuck. Live state of
/// `acquisitions.sync`, stored next to the progress. No state of its own:
/// the acquisition stays [`AcquisitionState::Downloading`], so transitions,
/// request states and notifications do not change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DownloadStage {
    /// The download is complete; the source waits for or runs the import.
    Importing,
    /// The source has the files, the category's consume services (Jellyfin,
    /// Emby) do not list them yet.
    Arriving,
}

names!(DownloadStage {
    Importing => "importing",
    Arriving => "arriving",
});

impl DownloadStage {
    /// The stage as shown: only a downloading acquisition has one.
    #[must_use]
    pub const fn of(acquisition: AcquisitionState, stored: Option<Self>) -> Option<Self> {
        match acquisition {
            AcquisitionState::Downloading => stored,
            _ => None,
        }
    }
}

/// What a member (and the admin overview) sees for an item or a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestState {
    PendingApproval,
    Approved,
    Submitted,
    /// Downloading, with progress.
    InProgress,
    PartiallyAvailable,
    Available,
    Rejected,
    Cancelled,
    Failed,
    /// An admin revoked it (spec 0004 R3).
    Revoked,
    /// It left the library and can be requested again (spec 0004 R4).
    Removed,
}

names!(RequestState {
    PendingApproval => "pending_approval",
    Approved => "approved",
    Submitted => "submitted",
    InProgress => "in_progress",
    PartiallyAvailable => "partially_available",
    Available => "available",
    Rejected => "rejected",
    Cancelled => "cancelled",
    Failed => "failed",
    Revoked => "revoked",
    Removed => "removed",
});

impl RequestState {
    /// Whether the member may hide a request in this state from their own
    /// list (spec 0004 R2): nothing more happens to it.
    #[must_use]
    pub const fn can_hide(self) -> bool {
        matches!(
            self,
            Self::Available | Self::Rejected | Self::Cancelled | Self::Revoked | Self::Removed
        )
    }

    /// Whether the item ended without its unit (withdrawn, cancelled,
    /// rejected, revoked, removed).
    const fn ended(self) -> bool {
        matches!(
            self,
            Self::Rejected | Self::Cancelled | Self::Revoked | Self::Removed
        )
    }
}

/// The state of one item: its decision, and once approved, its
/// acquisition's state.
#[must_use]
pub const fn item_state(decision: Decision, acquisition: Option<AcquisitionState>) -> RequestState {
    match (decision, acquisition) {
        (Decision::PendingApproval, _) => RequestState::PendingApproval,
        (Decision::Rejected, _) => RequestState::Rejected,
        (Decision::Cancelled, _) => RequestState::Cancelled,
        (Decision::Revoked, _) => RequestState::Revoked,
        (Decision::Removed, _) => RequestState::Removed,
        // A cancelled acquisition with an approved item is about to start
        // over (the approval re-activates it).
        (
            Decision::Approved,
            None | Some(AcquisitionState::Pending | AcquisitionState::Cancelled),
        ) => RequestState::Approved,
        (Decision::Approved, Some(AcquisitionState::Submitted)) => RequestState::Submitted,
        (Decision::Approved, Some(AcquisitionState::Downloading)) => RequestState::InProgress,
        (Decision::Approved, Some(AcquisitionState::PartiallyAvailable)) => {
            RequestState::PartiallyAvailable
        }
        (Decision::Approved, Some(AcquisitionState::Available)) => RequestState::Available,
        (Decision::Approved, Some(AcquisitionState::Failed)) => RequestState::Failed,
    }
}

/// The state of a request from the states of its items. Ended items
/// (withdrawn, cancelled, rejected, revoked, removed) count only when all
/// items ended; then the most telling ending wins: revoked, rejected,
/// removed, cancelled. Of the rest: available when all are; failed as soon
/// as one failed (it needs an admin, and the admin filter for failed
/// requests must find it, lesson 16); partially available as soon as
/// anything is watchable; then waiting approvals and the least advanced
/// progress.
#[must_use]
pub fn request_state(items: &[RequestState]) -> RequestState {
    let live: Vec<RequestState> = items.iter().copied().filter(|s| !s.ended()).collect();
    if live.is_empty() {
        return [
            RequestState::Revoked,
            RequestState::Rejected,
            RequestState::Removed,
        ]
        .into_iter()
        .find(|s| items.contains(s))
        .unwrap_or(RequestState::Cancelled);
    }
    let any = |state: RequestState| live.contains(&state);
    if live.iter().all(|s| *s == RequestState::Available) {
        RequestState::Available
    } else if any(RequestState::Failed) {
        RequestState::Failed
    } else if any(RequestState::Available) || any(RequestState::PartiallyAvailable) {
        RequestState::PartiallyAvailable
    } else if any(RequestState::PendingApproval) {
        RequestState::PendingApproval
    } else if any(RequestState::Approved) {
        RequestState::Approved
    } else if any(RequestState::Submitted) {
        RequestState::Submitted
    } else {
        RequestState::InProgress
    }
}

/// The stage of a request from its items' states and stages: only for a
/// request in progress, and only when every item in progress has one; then
/// the least advanced (one season still importing holds the request at
/// importing).
#[must_use]
pub fn request_stage(
    state: RequestState,
    items: &[(RequestState, Option<DownloadStage>)],
) -> Option<DownloadStage> {
    if state != RequestState::InProgress {
        return None;
    }
    // `None` sorts first, so one plain download makes the minimum `None`.
    items
        .iter()
        .filter(|(s, _)| *s == RequestState::InProgress)
        .map(|(_, stage)| *stage)
        .min()
        .flatten()
}

/// How a new request item enters the lifecycle (D6, D10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Waits for an admin.
    Pending,
    /// Approved by the member's request mode `auto`.
    Approved,
    /// Attached to an acquisition that is approved or running: counts as
    /// approved right away, whatever the request mode.
    Attached,
    /// The unit is available already; nothing to request.
    AlreadyAvailable,
}

impl Admission {
    /// The decision the item starts with; `None` when it is not created.
    #[must_use]
    pub const fn decision(self) -> Option<Decision> {
        match self {
            Self::Pending => Some(Decision::PendingApproval),
            Self::Approved | Self::Attached => Some(Decision::Approved),
            Self::AlreadyAvailable => None,
        }
    }
}

/// Admit a new item given the member's request mode, the unit's existing
/// acquisition at the category's source, if any, and whether an admin has
/// to decide again ([`needs_approval`]). A failed or cancelled acquisition
/// counts as none: the new request decides by its mode and, once approved,
/// starts the acquisition over. After a rejection or a revocation the
/// member may ask again, but an admin decides again: neither attaching nor
/// request mode `auto` approves it (D10, maintainer 2026-11-02; spec 0004).
#[must_use]
pub const fn admit(
    mode: RequestMode,
    existing: Option<AcquisitionState>,
    needs_approval: bool,
) -> Admission {
    match existing {
        Some(AcquisitionState::Available) => Admission::AlreadyAvailable,
        _ if needs_approval => Admission::Pending,
        Some(
            AcquisitionState::Pending
            | AcquisitionState::Submitted
            | AcquisitionState::Downloading
            | AcquisitionState::PartiallyAvailable,
        ) => Admission::Attached,
        Some(AcquisitionState::Failed | AcquisitionState::Cancelled) | None => match mode {
            RequestMode::Auto => Admission::Approved,
            RequestMode::Approval => Admission::Pending,
        },
    }
}

/// Whether a member's next request for a unit waits for an admin whatever
/// their mode: they were rejected for it (D10) or an admin revoked their
/// request for it (spec 0004), and the unit has not left the library since
/// (after removal it is new for everyone). `last_refusal` is when the
/// member's latest rejection or revocation for the unit happened,
/// `last_removal` when the unit was last removed (anyone's item).
#[must_use]
pub fn needs_approval<T: PartialOrd>(last_refusal: Option<T>, last_removal: Option<T>) -> bool {
    match (last_refusal, last_removal) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(refused), Some(removed)) => refused > removed,
    }
}

/// What consume services (Jellyfin, Emby) have of a unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumePresence {
    /// The category watches in no consume service: availability comes from
    /// the source's files.
    NoService,
    /// No consume service of the category has it.
    Absent,
    /// Present with this many episodes (movies: any value above zero).
    Present { episodes: u32 },
}

/// One unit as `acquisitions.sync` observed it.
#[derive(Debug, Clone, Copy)]
pub struct Observation<'a> {
    /// Files in the source: the movie, or this season.
    pub files: FileState,
    /// Seasons: episodes whose air date has passed, per the source.
    /// `None` for movies.
    pub aired: Option<u32>,
    /// The source's queue entries for this unit.
    pub queue: &'a [QueueEntry],
    pub consume: ConsumePresence,
}

/// The acquisition's new live state.
#[derive(Debug, Clone, PartialEq)]
pub struct Observed {
    pub state: AcquisitionState,
    /// Downloaded fraction in `0.0..=1.0`, if known.
    pub progress: Option<f64>,
    /// The source's explanation of a queue warning or failure.
    pub message: Option<String>,
    /// Downloading only: importing, or arriving in the consume service.
    pub stage: Option<DownloadStage>,
}

impl Observed {
    /// The source has the files but the consume services' inventory does
    /// not: `acquisitions.sync` asks for an early `inventory.sync` instead
    /// of waiting for the hourly one.
    #[must_use]
    pub fn refresh_library(&self) -> bool {
        self.stage == Some(DownloadStage::Arriving)
    }

    /// The download finished but the source reports a warning instead of
    /// importing it (Radarr/Sonarr "import blocked"): the admins are told
    /// once per acquisition (`import_blocked`). The same reading as the
    /// journey's "stuck at import".
    #[must_use]
    pub fn import_blocked(&self) -> bool {
        self.state == AcquisitionState::Downloading
            && self.stage.is_none()
            && self.message.is_some()
            && self.progress.is_some_and(|p| p >= 1.0)
    }
}

/// Every queue entry finished loading and waits for or runs the import.
fn importing(queue: &[QueueEntry]) -> bool {
    !queue.is_empty()
        && queue
            .iter()
            .all(|e| matches!(e.state, QueueState::ImportPending | QueueState::Importing))
}

/// Some queue entry still loads or needs attention (a blocked import shows
/// as a warning with the source's message, not as a stage).
fn loading(queue: &[QueueEntry]) -> bool {
    queue.iter().any(|e| {
        matches!(
            e.state,
            QueueState::Queued
                | QueueState::Paused
                | QueueState::Downloading
                | QueueState::Warning
                | QueueState::Failed
        )
    })
}

fn queue_progress(queue: &[QueueEntry]) -> Option<f64> {
    let size: u64 = queue.iter().map(|e| e.size_bytes).sum();
    if size == 0 {
        return None;
    }
    let left: u64 = queue
        .iter()
        .map(|e| e.size_left_bytes.min(e.size_bytes))
        .sum();
    #[allow(clippy::cast_precision_loss)] // display value only
    let done = (size - left) as f64 / size as f64;
    Some(done.clamp(0.0, 1.0))
}

fn queue_message(queue: &[QueueEntry]) -> Option<String> {
    queue
        .iter()
        .filter(|e| matches!(e.state, QueueState::Warning | QueueState::Failed))
        .find_map(|e| e.message.clone())
}

/// Availability of a unit (D15): from the consume services where the
/// category has any (the member watches there), else from the source's
/// files. A season is available when every aired episode is there.
const fn availability(o: &Observation<'_>) -> FileState {
    match o.consume {
        ConsumePresence::NoService => o.files,
        ConsumePresence::Absent | ConsumePresence::Present { episodes: 0 } => FileState::None,
        ConsumePresence::Present { episodes } => match o.aired {
            None => FileState::Complete,
            Some(aired) if aired > 0 && episodes >= aired => FileState::Complete,
            Some(_) => FileState::Partial,
        },
    }
}

/// Derive an acquisition's live state from one sync observation.
#[must_use]
pub fn observe(o: &Observation<'_>) -> Observed {
    let message = queue_message(o.queue);
    match availability(o) {
        FileState::Complete => Observed {
            state: AcquisitionState::Available,
            progress: Some(1.0),
            message: None,
            stage: None,
        },
        FileState::Partial => Observed {
            state: AcquisitionState::PartiallyAvailable,
            progress: queue_progress(o.queue),
            message,
            stage: None,
        },
        FileState::None if importing(o.queue) => Observed {
            state: AcquisitionState::Downloading,
            progress: queue_progress(o.queue),
            message,
            stage: Some(DownloadStage::Importing),
        },
        // Downloaded, not in the consume service yet. Entries that load
        // nothing any more (imported but still listed while seeding) do not
        // hold it back.
        FileState::None if o.files != FileState::None && !loading(o.queue) => Observed {
            state: AcquisitionState::Downloading,
            progress: Some(1.0),
            message: None,
            stage: Some(DownloadStage::Arriving),
        },
        FileState::None if !o.queue.is_empty() => Observed {
            state: AcquisitionState::Downloading,
            progress: queue_progress(o.queue),
            message,
            stage: None,
        },
        FileState::None => Observed {
            state: AcquisitionState::Submitted,
            progress: None,
            message: None,
            stage: None,
        },
    }
}

/// What a member is shown for a title or season before requesting (user
/// story 2): never who requested it (D10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Availability {
    /// Nobody asked for it and it is not there.
    New,
    /// Someone asked for it (open request or running acquisition).
    Requested,
    PartiallyAvailable,
    Available,
}

names!(Availability {
    New => "new",
    Requested => "requested",
    PartiallyAvailable => "partially_available",
    Available => "available",
});

/// The availability of one unit. `expected_episodes` is the season's
/// episode count from the catalog (`None` for movies): without the source's
/// aired count at hand, the web side calls a season available when the
/// consume services have that many episodes.
#[must_use]
pub const fn unit_availability(
    acquisition: Option<AcquisitionState>,
    open_requests: bool,
    presence: ConsumePresence,
    expected_episodes: Option<u32>,
) -> Availability {
    if matches!(acquisition, Some(AcquisitionState::Available)) {
        return Availability::Available;
    }
    if let ConsumePresence::Present { episodes } = presence
        && episodes > 0
    {
        return match expected_episodes {
            None => Availability::Available,
            Some(expected) if expected > 0 && episodes >= expected => Availability::Available,
            Some(_) => Availability::PartiallyAvailable,
        };
    }
    match acquisition {
        Some(AcquisitionState::PartiallyAvailable) => Availability::PartiallyAvailable,
        Some(
            AcquisitionState::Pending
            | AcquisitionState::Submitted
            | AcquisitionState::Downloading
            | AcquisitionState::Failed,
        ) => Availability::Requested,
        _ if open_requests => Availability::Requested,
        _ => Availability::New,
    }
}

/// The availability of a series from its regular seasons' (specials
/// excluded by the caller): available when all are, partially available as
/// soon as one has something, requested when any is.
#[must_use]
pub fn series_availability(seasons: &[Availability]) -> Availability {
    if seasons.is_empty() {
        return Availability::New;
    }
    if seasons.iter().all(|s| *s == Availability::Available) {
        Availability::Available
    } else if seasons.iter().any(|s| {
        matches!(
            s,
            Availability::Available | Availability::PartiallyAvailable
        )
    }) {
        Availability::PartiallyAvailable
    } else if seasons.contains(&Availability::Requested) {
        Availability::Requested
    } else {
        Availability::New
    }
}

/// The availability of an artist from its albums Starr knows (those
/// someone requested, spec 0006 slice 4): partially available as soon as
/// one has something (Starr never knows the whole discography to be
/// there), requested when any is, else new.
#[must_use]
pub fn artist_availability(albums: &[Availability]) -> Availability {
    if albums.iter().any(|a| {
        matches!(
            a,
            Availability::Available | Availability::PartiallyAvailable
        )
    }) {
        Availability::PartiallyAvailable
    } else if albums.contains(&Availability::Requested) {
        Availability::Requested
    } else {
        Availability::New
    }
}

/// Submission attempts per acquisition before it fails for good.
pub const SUBMIT_MAX_ATTEMPTS: u32 = 5;
/// Delay before the first resubmission; doubles with every further one.
pub const SUBMIT_BACKOFF: TimeDelta = TimeDelta::minutes(1);

/// When to try a failed submission again: `None` when the error is
/// permanent (credentials, refusal, version, unknown title) or the
/// attempts are used up. `attempts` counts the attempts made so far,
/// including the failed one.
#[must_use]
pub fn resubmit_after(error: &AdapterError, attempts: u32) -> Option<TimeDelta> {
    if attempts >= SUBMIT_MAX_ATTEMPTS {
        return None;
    }
    let backoff = SUBMIT_BACKOFF * 2i32.saturating_pow(attempts.saturating_sub(1));
    match error {
        AdapterError::Transient(_) => Some(backoff),
        AdapterError::Throttled { retry_after } => Some(
            retry_after
                .and_then(|d| TimeDelta::from_std(d.max(Duration::from_secs(1))).ok())
                .map_or(backoff, |d| d.max(backoff)),
        ),
        AdapterError::CredentialsInvalid
        | AdapterError::NotFound(_)
        | AdapterError::Rejected(_)
        | AdapterError::VersionUnsupported { .. }
        | AdapterError::Unknown(_) => None,
    }
}

/// A reject reason as stored: trimmed, non-empty, at most
/// [`REJECT_REASON_MAX_CHARS`] characters.
#[must_use]
pub fn reject_reason(input: &str) -> Option<&str> {
    let reason = input.trim();
    (!reason.is_empty() && reason.chars().count() <= REJECT_REASON_MAX_CHARS).then_some(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    use AcquisitionState as A;
    use RequestState as S;

    #[test]
    fn names_round_trip() {
        for d in Decision::ALL {
            assert_eq!(Decision::parse(d.as_str()), Some(d));
        }
        for a in A::ALL {
            assert_eq!(A::parse(a.as_str()), Some(a));
        }
        for s in S::ALL {
            assert_eq!(S::parse(s.as_str()), Some(s));
        }
        assert_eq!(S::parse("downloading"), None);
        assert_eq!(S::InProgress.as_str(), "in_progress");
    }

    #[test]
    fn only_pending_items_can_be_decided() {
        use DecisionAction::{Approve, Reject, Withdraw};
        assert_eq!(
            decide(Decision::PendingApproval, Approve),
            Ok(Decision::Approved)
        );
        assert_eq!(
            decide(Decision::PendingApproval, Reject),
            Ok(Decision::Rejected)
        );
        assert_eq!(
            decide(Decision::PendingApproval, Withdraw),
            Ok(Decision::Cancelled)
        );
        for from in [
            Decision::Approved,
            Decision::Rejected,
            Decision::Cancelled,
            Decision::Revoked,
            Decision::Removed,
        ] {
            for action in [Approve, Reject, Withdraw] {
                assert_eq!(decide(from, action), Err(TransitionError { from, action }));
            }
        }
    }

    /// Spec 0008 F-4: only an approval goes back to waiting; a rejection
    /// stays (0003 D11), and nothing else can be undone.
    #[test]
    fn only_an_approval_can_be_undone() {
        use Decision::{Approved, Cancelled, PendingApproval, Rejected, Removed, Revoked};
        assert_eq!(
            decide(Approved, DecisionAction::UndoApprove),
            Ok(PendingApproval)
        );
        for from in [PendingApproval, Rejected, Cancelled, Revoked, Removed] {
            assert!(decide(from, DecisionAction::UndoApprove).is_err());
        }
    }

    #[test]
    fn open_items_end_by_cancel_revoke_or_removal() {
        use Decision::{Approved, Cancelled, PendingApproval, Rejected, Removed, Revoked};
        use DecisionAction::{Cancel, Remove, Revoke};
        // Cancel and removal only after approval (withdraw covers pending).
        assert_eq!(decide(Approved, Cancel), Ok(Cancelled));
        assert!(decide(PendingApproval, Cancel).is_err());
        assert_eq!(decide(Approved, Remove), Ok(Removed));
        assert!(decide(PendingApproval, Remove).is_err());
        // Admins revoke anything open.
        assert_eq!(decide(PendingApproval, Revoke), Ok(Revoked));
        assert_eq!(decide(Approved, Revoke), Ok(Revoked));
        // Ended items stay ended.
        for from in [Rejected, Cancelled, Revoked, Removed] {
            for action in [Cancel, Revoke, Remove] {
                assert_eq!(decide(from, action), Err(TransitionError { from, action }));
            }
            assert!(!from.is_open());
        }
        assert!(PendingApproval.is_open() && Approved.is_open());
    }

    #[test]
    fn members_cancel_until_the_download_starts() {
        use Decision::{Approved, Cancelled, PendingApproval, Rejected, Removed, Revoked};
        use DecisionAction::{Cancel, Withdraw};
        assert_eq!(member_end(PendingApproval, None), Some(Withdraw));
        assert_eq!(member_end(Approved, None), Some(Cancel));
        for not_started in [A::Pending, A::Failed, A::Submitted, A::Cancelled] {
            assert_eq!(
                member_end(Approved, Some(not_started)),
                Some(Cancel),
                "{not_started:?}"
            );
        }
        for started in [A::Downloading, A::PartiallyAvailable, A::Available] {
            assert_eq!(member_end(Approved, Some(started)), None, "{started:?}");
        }
        for ended in [Rejected, Cancelled, Revoked, Removed] {
            assert_eq!(member_end(ended, Some(A::Pending)), None, "{ended:?}");
        }
    }

    #[test]
    fn unwanted_acquisitions_are_cancelled_stopped_or_kept() {
        use DownloadStage::{Arriving, Importing};
        use Unwanted::{Cancel, Keep, Stop};
        // Not sent yet (pending, held) or failed: cancelled, no write.
        for monitored in [false, true] {
            assert_eq!(A::Pending.when_unwanted(None, monitored), Cancel);
            assert_eq!(A::Failed.when_unwanted(None, monitored), Cancel);
        }
        // Searching: Starr turns off what it turned on, nothing else.
        assert_eq!(A::Submitted.when_unwanted(None, true), Stop);
        assert_eq!(A::Submitted.when_unwanted(None, false), Cancel);
        // Downloading or importing: stop searching, leave the download.
        assert_eq!(A::Downloading.when_unwanted(None, true), Stop);
        assert_eq!(A::Downloading.when_unwanted(Some(Importing), true), Stop);
        assert_eq!(A::Downloading.when_unwanted(None, false), Keep);
        // Files there: nothing in the source.
        assert_eq!(A::Downloading.when_unwanted(Some(Arriving), true), Keep);
        for state in [A::PartiallyAvailable, A::Available, A::Cancelled] {
            assert_eq!(state.when_unwanted(None, true), Keep, "{state:?}");
        }
    }

    #[test]
    fn only_settled_requests_can_be_hidden() {
        for s in S::ALL {
            let expected = matches!(
                s,
                S::Available | S::Rejected | S::Cancelled | S::Revoked | S::Removed
            );
            assert_eq!(s.can_hide(), expected, "{s:?}");
        }
    }

    #[test]
    fn revoked_or_rejected_members_ask_an_admin_until_the_unit_was_removed() {
        assert!(!needs_approval::<u32>(None, None));
        assert!(!needs_approval(None, Some(5)));
        assert!(needs_approval(Some(3), None));
        // Removed after the refusal: new for everyone.
        assert!(!needs_approval(Some(3), Some(5)));
        // Refused again after the removal.
        assert!(needs_approval(Some(7), Some(5)));
    }

    #[test]
    fn acquisition_transitions() {
        assert!(A::Pending.can_move_to(A::Submitted));
        assert!(A::Pending.can_move_to(A::Failed));
        assert!(!A::Pending.can_move_to(A::Available));
        assert!(A::Downloading.can_move_to(A::Available));
        assert!(A::PartiallyAvailable.can_move_to(A::Downloading));
        assert!(!A::Submitted.can_move_to(A::Pending));
        assert!(A::Failed.can_move_to(A::Pending));
        assert!(!A::Failed.can_move_to(A::Submitted));
        // Available only leaves when its unit left the library.
        for to in A::ALL {
            assert_eq!(A::Available.can_move_to(to), to == A::Cancelled, "{to:?}");
        }
        // Cancelled starts over on a new approval, nothing else.
        for to in A::ALL {
            assert_eq!(A::Cancelled.can_move_to(to), to == A::Pending, "{to:?}");
        }
        for from in [A::Pending, A::Submitted, A::Downloading, A::Failed] {
            assert!(from.can_move_to(A::Cancelled), "{from:?}");
        }
        assert!(!A::Cancelled.is_synced());
        assert!(!A::Cancelled.is_held(true));
        assert!(A::Downloading.is_synced());
        assert!(!A::Pending.is_synced());
        assert!(!A::Available.is_synced());
    }

    #[test]
    fn only_pending_acquisitions_are_held_by_paused_writes() {
        assert!(A::Pending.is_held(true));
        assert!(!A::Pending.is_held(false));
        for state in A::ALL.into_iter().filter(|s| *s != A::Pending) {
            assert!(!state.is_held(true), "{state:?}");
        }
        // Held is still pending to the member: approved, not yet submitted.
        assert_eq!(
            item_state(Decision::Approved, Some(A::Pending)),
            S::Approved
        );
    }

    #[test]
    fn item_state_follows_the_acquisition_once_approved() {
        assert_eq!(
            item_state(Decision::PendingApproval, None),
            S::PendingApproval
        );
        // A rejected item stays rejected even when the title arrives (D10).
        assert_eq!(
            item_state(Decision::Rejected, Some(A::Available)),
            S::Rejected
        );
        assert_eq!(item_state(Decision::Cancelled, None), S::Cancelled);
        assert_eq!(
            item_state(Decision::Cancelled, Some(A::Submitted)),
            S::Cancelled
        );
        assert_eq!(
            item_state(Decision::Revoked, Some(A::Available)),
            S::Revoked
        );
        assert_eq!(
            item_state(Decision::Removed, Some(A::Cancelled)),
            S::Removed
        );
        // Approved on a cancelled acquisition: about to start over.
        assert_eq!(
            item_state(Decision::Approved, Some(A::Cancelled)),
            S::Approved
        );
        assert_eq!(item_state(Decision::Approved, None), S::Approved);
        assert_eq!(
            item_state(Decision::Approved, Some(A::Pending)),
            S::Approved
        );
        assert_eq!(
            item_state(Decision::Approved, Some(A::Submitted)),
            S::Submitted
        );
        assert_eq!(
            item_state(Decision::Approved, Some(A::Downloading)),
            S::InProgress
        );
        assert_eq!(
            item_state(Decision::Approved, Some(A::PartiallyAvailable)),
            S::PartiallyAvailable
        );
        assert_eq!(
            item_state(Decision::Approved, Some(A::Available)),
            S::Available
        );
        assert_eq!(item_state(Decision::Approved, Some(A::Failed)), S::Failed);
    }

    #[test]
    fn request_state_aggregates_items() {
        assert_eq!(request_state(&[]), S::Cancelled);
        assert_eq!(request_state(&[S::Cancelled, S::Cancelled]), S::Cancelled);
        assert_eq!(request_state(&[S::Cancelled, S::Rejected]), S::Rejected);
        // Endings: revoked over rejected over removed over cancelled.
        assert_eq!(request_state(&[S::Rejected, S::Revoked]), S::Revoked);
        assert_eq!(request_state(&[S::Cancelled, S::Revoked]), S::Revoked);
        assert_eq!(request_state(&[S::Removed, S::Rejected]), S::Rejected);
        assert_eq!(request_state(&[S::Cancelled, S::Removed]), S::Removed);
        // An ended season does not hide a live one.
        assert_eq!(request_state(&[S::Revoked, S::InProgress]), S::InProgress);
        assert_eq!(request_state(&[S::Removed, S::Available]), S::Available);
        assert_eq!(request_state(&[S::Available]), S::Available);
        assert_eq!(request_state(&[S::Available, S::Rejected]), S::Available);
        assert_eq!(
            request_state(&[S::Available, S::InProgress]),
            S::PartiallyAvailable
        );
        assert_eq!(
            request_state(&[S::PartiallyAvailable]),
            S::PartiallyAvailable
        );
        assert_eq!(request_state(&[S::Failed, S::InProgress]), S::Failed);
        // A failed season is not hidden behind an available one.
        assert_eq!(request_state(&[S::Available, S::Failed]), S::Failed);
        assert_eq!(
            request_state(&[S::PartiallyAvailable, S::Failed]),
            S::Failed
        );
        assert_eq!(
            request_state(&[S::PendingApproval, S::InProgress]),
            S::PendingApproval
        );
        assert_eq!(request_state(&[S::Approved, S::InProgress]), S::Approved);
        assert_eq!(request_state(&[S::Submitted, S::InProgress]), S::Submitted);
        assert_eq!(request_state(&[S::InProgress]), S::InProgress);
        // Every state of a single item is its own aggregate.
        for s in S::ALL {
            assert_eq!(request_state(&[s]), s);
        }
    }

    #[test]
    fn admission_attaches_to_running_acquisitions() {
        use RequestMode::{Approval, Auto};
        assert_eq!(admit(Approval, None, false), Admission::Pending);
        assert_eq!(admit(Auto, None, false), Admission::Approved);
        for running in [
            A::Pending,
            A::Submitted,
            A::Downloading,
            A::PartiallyAvailable,
        ] {
            assert_eq!(admit(Approval, Some(running), false), Admission::Attached);
            assert_eq!(admit(Auto, Some(running), false), Admission::Attached);
        }
        assert_eq!(admit(Approval, Some(A::Failed), false), Admission::Pending);
        assert_eq!(admit(Auto, Some(A::Failed), false), Admission::Approved);
        // A cancelled acquisition is nobody's: the new request's mode decides.
        assert_eq!(
            admit(Approval, Some(A::Cancelled), false),
            Admission::Pending
        );
        assert_eq!(admit(Auto, Some(A::Cancelled), false), Admission::Approved);
        assert_eq!(
            admit(Auto, Some(A::Available), false),
            Admission::AlreadyAvailable
        );
        assert_eq!(Admission::Attached.decision(), Some(Decision::Approved));
        // After a rejection an admin decides again, whatever the mode or
        // a running acquisition (D10); available stays available.
        for existing in [
            None,
            Some(A::Pending),
            Some(A::Downloading),
            Some(A::Failed),
        ] {
            assert_eq!(
                admit(Auto, existing, true),
                Admission::Pending,
                "{existing:?}"
            );
            assert_eq!(
                admit(Approval, existing, true),
                Admission::Pending,
                "{existing:?}"
            );
        }
        assert_eq!(
            admit(Auto, Some(A::Available), true),
            Admission::AlreadyAvailable
        );
        assert_eq!(
            Admission::Pending.decision(),
            Some(Decision::PendingApproval)
        );
        assert_eq!(Admission::AlreadyAvailable.decision(), None);
    }

    fn entry(size: u64, left: u64, state: QueueState, message: Option<&str>) -> QueueEntry {
        QueueEntry {
            unit: None,
            size_bytes: size,
            size_left_bytes: left,
            state,
            message: message.map(str::to_owned),
        }
    }

    fn obs(
        files: FileState,
        aired: Option<u32>,
        queue: &[QueueEntry],
        consume: ConsumePresence,
    ) -> Observation<'_> {
        Observation {
            files,
            aired,
            queue,
            consume,
        }
    }

    #[test]
    fn without_consume_service_files_decide_availability() {
        let none = ConsumePresence::NoService;
        let o = observe(&obs(FileState::Complete, None, &[], none));
        assert_eq!(o.state, A::Available);
        assert_eq!(o.progress, Some(1.0));
        assert_eq!(
            observe(&obs(FileState::Partial, Some(8), &[], none)).state,
            A::PartiallyAvailable
        );
        assert_eq!(
            observe(&obs(FileState::None, None, &[], none)).state,
            A::Submitted
        );
    }

    #[test]
    fn with_consume_service_only_the_consume_service_makes_it_available() {
        // Radarr has the file, Jellyfin has not scanned it yet.
        let o = observe(&obs(
            FileState::Complete,
            None,
            &[],
            ConsumePresence::Absent,
        ));
        assert_eq!(o.state, A::Downloading);
        assert_eq!(o.progress, Some(1.0));
        let movie = observe(&obs(
            FileState::Complete,
            None,
            &[],
            ConsumePresence::Present { episodes: 1 },
        ));
        assert_eq!(movie.state, A::Available);
        // D15: partially from the first episode, available with all aired.
        let season = |episodes| {
            observe(&obs(
                FileState::Partial,
                Some(10),
                &[],
                ConsumePresence::Present { episodes },
            ))
            .state
        };
        assert_eq!(season(1), A::PartiallyAvailable);
        assert_eq!(season(9), A::PartiallyAvailable);
        assert_eq!(season(10), A::Available);
        assert_eq!(season(0), A::Downloading);
        // Nothing aired yet: an early episode is partial, never complete.
        let early = observe(&obs(
            FileState::Partial,
            Some(0),
            &[],
            ConsumePresence::Present { episodes: 1 },
        ));
        assert_eq!(early.state, A::PartiallyAvailable);
    }

    #[test]
    fn queue_gives_progress_and_the_sources_message() {
        let queue = [
            entry(100, 50, QueueState::Downloading, None),
            entry(
                300,
                50,
                QueueState::Warning,
                Some("No files found are eligible for import"),
            ),
        ];
        let o = observe(&obs(
            FileState::None,
            None,
            &queue,
            ConsumePresence::NoService,
        ));
        assert_eq!(o.state, A::Downloading);
        assert_eq!(o.progress, Some(0.75));
        assert_eq!(
            o.message.as_deref(),
            Some("No files found are eligible for import")
        );
        let unknown = [entry(0, 0, QueueState::Queued, None)];
        let o = observe(&obs(
            FileState::None,
            None,
            &unknown,
            ConsumePresence::NoService,
        ));
        assert_eq!((o.state, o.progress), (A::Downloading, None));
    }

    #[test]
    fn a_finished_download_with_a_warning_is_a_blocked_import() {
        let blocked = [entry(
            100,
            0,
            QueueState::Warning,
            Some("No files found are eligible for import"),
        )];
        let o = observe(&obs(
            FileState::None,
            None,
            &blocked,
            ConsumePresence::Absent,
        ));
        assert!(o.import_blocked(), "{o:?}");
        // Still loading with a warning (stalled): not an import problem.
        let stalled = [entry(100, 40, QueueState::Warning, Some("Stalled"))];
        let o = observe(&obs(
            FileState::None,
            None,
            &stalled,
            ConsumePresence::Absent,
        ));
        assert!(!o.import_blocked(), "{o:?}");
        // Waiting for or in the import without a warning: fine.
        let pending = [entry(100, 0, QueueState::ImportPending, None)];
        let o = observe(&obs(
            FileState::None,
            None,
            &pending,
            ConsumePresence::Absent,
        ));
        assert!(!o.import_blocked(), "{o:?}");
        // Available after all: nothing blocks.
        let o = observe(&obs(
            FileState::Complete,
            None,
            &blocked,
            ConsumePresence::NoService,
        ));
        assert!(!o.import_blocked(), "{o:?}");
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one table of cases
    fn stage_tells_importing_and_arriving_apart_from_downloading() {
        use DownloadStage::{Arriving, Importing};
        use QueueState::{Downloading, ImportPending, Importing as Imp, Queued, Warning};
        let jellyfin = ConsumePresence::Absent;
        let none = ConsumePresence::NoService;
        let done = |state| entry(100, 0, state, None);
        // (files, queue, consume) → (state, stage, progress)
        let cases = [
            // Still downloading: progress, no stage.
            (
                FileState::None,
                vec![entry(100, 40, Downloading, None)],
                jellyfin,
                A::Downloading,
                None,
                Some(0.6),
            ),
            (
                FileState::None,
                vec![entry(100, 100, Queued, None)],
                jellyfin,
                A::Downloading,
                None,
                Some(0.0),
            ),
            // Completed in the queue, waiting for or in the import.
            (
                FileState::None,
                vec![done(ImportPending)],
                jellyfin,
                A::Downloading,
                Some(Importing),
                Some(1.0),
            ),
            (
                FileState::None,
                vec![done(Imp)],
                none,
                A::Downloading,
                Some(Importing),
                Some(1.0),
            ),
            // One entry still loading: downloading wins.
            (
                FileState::None,
                vec![done(Imp), entry(100, 50, Downloading, None)],
                jellyfin,
                A::Downloading,
                None,
                Some(0.75),
            ),
            // A blocked import is a warning with the source's message.
            (
                FileState::None,
                vec![entry(100, 0, Warning, Some("blocked"))],
                jellyfin,
                A::Downloading,
                None,
                Some(1.0),
            ),
            // In the source, not in Jellyfin yet (the reported case).
            (
                FileState::Complete,
                vec![],
                jellyfin,
                A::Downloading,
                Some(Arriving),
                Some(1.0),
            ),
            (
                FileState::Partial,
                vec![],
                jellyfin,
                A::Downloading,
                Some(Arriving),
                Some(1.0),
            ),
            // Importing an upgrade while the old file is there: importing.
            (
                FileState::Complete,
                vec![done(Imp)],
                jellyfin,
                A::Downloading,
                Some(Importing),
                Some(1.0),
            ),
            // Imported, but the entry is still listed (seeding): arriving.
            (
                FileState::Complete,
                vec![done(QueueState::Other)],
                jellyfin,
                A::Downloading,
                Some(Arriving),
                Some(1.0),
            ),
            // The file is there but a new download still loads: downloading.
            (
                FileState::Complete,
                vec![entry(100, 50, Downloading, None)],
                jellyfin,
                A::Downloading,
                None,
                Some(0.5),
            ),
            // An unknown entry without a file says nothing: downloading.
            (
                FileState::None,
                vec![done(QueueState::Other)],
                jellyfin,
                A::Downloading,
                None,
                Some(1.0),
            ),
            // Nothing anywhere: submitted, no stage.
            (FileState::None, vec![], jellyfin, A::Submitted, None, None),
        ];
        for (files, queue, consume, state, stage, progress) in cases {
            let o = observe(&obs(files, None, &queue, consume));
            assert_eq!(
                (o.state, o.stage, o.progress),
                (state, stage, progress),
                "{files:?} {queue:?} {consume:?}"
            );
            assert_eq!(o.refresh_library(), stage == Some(Arriving));
        }
        // Available and partially available carry no stage.
        let present = ConsumePresence::Present { episodes: 1 };
        assert_eq!(
            observe(&obs(FileState::Complete, None, &[], present)).stage,
            None
        );
        let partial = observe(&obs(FileState::Partial, Some(4), &[done(Imp)], present));
        assert_eq!(
            (partial.state, partial.stage),
            (A::PartiallyAvailable, None)
        );
    }

    #[test]
    fn stage_only_counts_while_downloading() {
        use DownloadStage::{Arriving, Importing};
        assert_eq!(
            DownloadStage::of(A::Downloading, Some(Arriving)),
            Some(Arriving)
        );
        assert_eq!(DownloadStage::of(A::Downloading, None), None);
        for state in A::ALL.into_iter().filter(|s| *s != A::Downloading) {
            assert_eq!(DownloadStage::of(state, Some(Importing)), None, "{state:?}");
        }
        for stage in DownloadStage::ALL {
            assert_eq!(DownloadStage::parse(stage.as_str()), Some(stage));
        }
    }

    #[test]
    fn a_request_shows_the_least_advanced_stage_of_its_downloads() {
        use DownloadStage::{Arriving, Importing};
        let stage = |items: &[(RequestState, Option<DownloadStage>)]| {
            let states: Vec<RequestState> = items.iter().map(|i| i.0).collect();
            request_stage(request_state(&states), items)
        };
        assert_eq!(stage(&[(S::InProgress, Some(Arriving))]), Some(Arriving));
        assert_eq!(
            stage(&[
                (S::InProgress, Some(Arriving)),
                (S::InProgress, Some(Importing))
            ]),
            Some(Importing)
        );
        assert_eq!(
            stage(&[(S::InProgress, Some(Arriving)), (S::InProgress, None)]),
            None
        );
        // Rejected seasons do not hold it back.
        assert_eq!(
            stage(&[(S::InProgress, Some(Arriving)), (S::Rejected, None)]),
            Some(Arriving)
        );
        // Not in progress as a whole: no stage.
        assert_eq!(
            stage(&[(S::InProgress, Some(Arriving)), (S::Submitted, None)]),
            None
        );
        assert_eq!(
            stage(&[(S::Available, None), (S::InProgress, Some(Arriving))]),
            None
        );
        assert_eq!(stage(&[]), None);
    }

    #[test]
    fn unit_availability_prefers_evidence_of_files() {
        use Availability::{Available, New, PartiallyAvailable, Requested};
        let none = ConsumePresence::NoService;
        assert_eq!(unit_availability(None, false, none, None), New);
        assert_eq!(unit_availability(None, true, none, None), Requested);
        assert_eq!(
            unit_availability(Some(A::Downloading), false, none, None),
            Requested
        );
        assert_eq!(
            unit_availability(Some(A::Failed), false, none, None),
            Requested
        );
        // Cancelled (nobody wants it, or it left the library): new again.
        assert_eq!(
            unit_availability(Some(A::Cancelled), false, none, None),
            New
        );
        assert_eq!(
            unit_availability(Some(A::Cancelled), true, none, None),
            Requested
        );
        assert_eq!(
            unit_availability(Some(A::PartiallyAvailable), true, none, Some(10)),
            PartiallyAvailable
        );
        assert_eq!(
            unit_availability(Some(A::Available), true, none, None),
            Available
        );
        // Already in Jellyfin without any request (D14: existing library).
        let present = |episodes| ConsumePresence::Present { episodes };
        assert_eq!(unit_availability(None, false, present(1), None), Available);
        assert_eq!(
            unit_availability(None, false, present(4), Some(10)),
            PartiallyAvailable
        );
        assert_eq!(
            unit_availability(None, false, present(10), Some(10)),
            Available
        );
        assert_eq!(
            unit_availability(None, true, present(0), Some(10)),
            Requested
        );
        assert_eq!(
            unit_availability(None, false, ConsumePresence::Absent, None),
            New
        );
    }

    #[test]
    fn series_availability_from_seasons() {
        use Availability::{Available, New, PartiallyAvailable, Requested};
        assert_eq!(series_availability(&[]), New);
        assert_eq!(series_availability(&[Available, Available]), Available);
        assert_eq!(series_availability(&[Available, New]), PartiallyAvailable);
        assert_eq!(series_availability(&[Requested, New]), Requested);
        assert_eq!(series_availability(&[New, New]), New);
        for a in Availability::ALL {
            assert_eq!(Availability::parse(a.as_str()), Some(a));
        }
    }

    #[test]
    fn an_artist_is_never_available_as_a_whole() {
        use Availability::{Available, New, PartiallyAvailable, Requested};
        assert_eq!(artist_availability(&[]), New);
        // Starr knows only the albums someone asked for: all of those being
        // there is still only part of the artist.
        assert_eq!(artist_availability(&[Available]), PartiallyAvailable);
        assert_eq!(
            artist_availability(&[PartiallyAvailable, New]),
            PartiallyAvailable
        );
        assert_eq!(artist_availability(&[Requested, New]), Requested);
        assert_eq!(artist_availability(&[New, New]), New);
    }

    #[test]
    fn only_transient_failures_are_resubmitted() {
        let transient = AdapterError::Transient("timeout".into());
        assert_eq!(resubmit_after(&transient, 1), Some(TimeDelta::minutes(1)));
        assert_eq!(resubmit_after(&transient, 3), Some(TimeDelta::minutes(4)));
        assert_eq!(resubmit_after(&transient, SUBMIT_MAX_ATTEMPTS), None);
        let throttled = AdapterError::Throttled {
            retry_after: Some(Duration::from_secs(600)),
        };
        assert_eq!(resubmit_after(&throttled, 1), Some(TimeDelta::minutes(10)));
        for permanent in [
            AdapterError::CredentialsInvalid,
            AdapterError::Rejected("no".into()),
            AdapterError::NotFound("x".into()),
            AdapterError::Unknown("?".into()),
        ] {
            assert_eq!(resubmit_after(&permanent, 1), None, "{permanent:?}");
        }
    }

    #[test]
    fn reject_reasons_are_trimmed_and_bounded() {
        assert_eq!(reject_reason("  not in 4K  "), Some("not in 4K"));
        assert_eq!(reject_reason("   "), None);
        assert_eq!(
            reject_reason(&"x".repeat(REJECT_REASON_MAX_CHARS)).map(str::len),
            Some(500)
        );
        assert_eq!(
            reject_reason(&"x".repeat(REJECT_REASON_MAX_CHARS + 1)),
            None
        );
    }
}
