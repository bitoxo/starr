//! Background jobs: the registry of job kinds, run states and run reports
//! (ADR 0004). Pure data; the queue lives in `starr-db`, execution in
//! `starr-worker`.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Every kind of background job Starr knows. The single registry (lesson 18):
/// schedule, timeout, retries and listing all derive from this enum through
/// exhaustive `match`es, so a new kind cannot be forgotten anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// Deletes old run history.
    Housekeeping,
    /// Does nothing and reports success; proves the pipeline end to end.
    Noop,
    /// Probes service instances: one (payload `{"instanceId": n}`) or all
    /// (empty payload).
    ServicesCheck,
    /// Reads quality profiles and root folders of one source instance
    /// (payload `{"instanceId": n}`) for the category settings (D9).
    SourcesLists,
    /// Reads the libraries of consume services (Jellyfin, Emby) and updates
    /// the inventory by provider IDs (spec 0003): one instance (payload
    /// `{"instanceId": n}`) or all (empty payload).
    InventorySync,
    /// Hands approved acquisitions to their source (spec 0003 slice 4):
    /// adds titles or extends season monitoring, one title at a time, and
    /// reads back. Enqueued on approval; scheduled as a catch-up for
    /// resubmissions after transient failures.
    RequestsSubmit,
    /// Follows submitted acquisitions: queue progress and file state from
    /// the source, availability from the consume services' inventory.
    AcquisitionsSync,
    /// Sends due notification deliveries through their channels (email,
    /// webhook, Discord), each with its own retries; or, with payload
    /// `{"testChannelId": n}`, a test message on one channel.
    NotificationsDeliver,
    /// Daily per follow (hourly runs for the follows due): new seasons of
    /// a followed series (listed by the catalog with an air date after the
    /// follow's cutoff) and new albums of a followed artist (first released
    /// after the follow date, of the category's release types) become
    /// requests in the member's name with their request mode (spec 0003
    /// D8, spec 0006 Q3, domain model §6).
    FollowsCheck,
    /// Reads the accounts of consume services that implement accounts
    /// (Jellyfin, Emby) into the foreign account inventory (spec 0005 D7,
    /// slice 1): every consume instance with accounts. Reads only; writes
    /// nothing to the services.
    AccountsScan,
    /// Plans the accounts of every consume instance with accounts from
    /// the desired state and the last scan (spec 0005 slice 3) and stores
    /// the plan per instance. Plan only: reads the database, calls no
    /// service. Chained after `accounts.scan` and enqueued on changes
    /// (entitlements, member status, library mapping, roles).
    AccountsReconcile,
    /// Writes members' passwords from their password handoffs (spec 0005
    /// slice 5, ADR 0010): creates the accounts that waited only for a
    /// password and writes the password where it is due, through the same
    /// safe-mode gate as `accounts.reconcile`; deletes each handoff when
    /// done. Every handoff in flight in one run; the payload is empty (no
    /// member, no secret). Enqueued when a member enters their password and
    /// after live plan runs while handoffs wait.
    AccountsPassword,
    /// Refreshes the catalog lists that are fetched in the background
    /// rather than while a member waits (spec 0008 slice 2): the music
    /// category's genre lanes (one `MusicBrainz` search per genre of a fixed
    /// list, F-8) at the worker's share of the budget, cached a day. The
    /// web process only reads them from the cache.
    CatalogRefreshLists,
}

/// Static description of a job kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobSpec {
    /// Hard limit for one execution; the handler is cancelled after it.
    pub timeout: Duration,
    /// Total executions including the first one (>= 1).
    pub max_attempts: u32,
    /// Delay before the first retry; doubles with every further attempt.
    pub backoff: Duration,
    /// If set, the scheduler enqueues the job at this interval.
    pub schedule: Option<Duration>,
}

impl JobKind {
    /// All kinds, in display order.
    pub const ALL: [Self; 13] = [
        Self::Housekeeping,
        Self::Noop,
        Self::ServicesCheck,
        Self::SourcesLists,
        Self::InventorySync,
        Self::RequestsSubmit,
        Self::AcquisitionsSync,
        Self::NotificationsDeliver,
        Self::FollowsCheck,
        Self::AccountsScan,
        Self::AccountsReconcile,
        Self::AccountsPassword,
        Self::CatalogRefreshLists,
    ];

    /// Stable name used in the database and the API.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Housekeeping => "system.housekeeping",
            Self::Noop => "system.noop",
            Self::ServicesCheck => "services.check",
            Self::SourcesLists => "sources.lists",
            Self::InventorySync => "inventory.sync",
            Self::RequestsSubmit => "requests.submit",
            Self::AcquisitionsSync => "acquisitions.sync",
            Self::NotificationsDeliver => "notifications.deliver",
            Self::FollowsCheck => "follows.check",
            Self::AccountsScan => "accounts.scan",
            Self::AccountsReconcile => "accounts.reconcile",
            Self::AccountsPassword => "accounts.password",
            Self::CatalogRefreshLists => "catalog.refresh-lists",
        }
    }

    /// Inverse of [`JobKind::name`]. `None` for names this binary does not
    /// know (for example written by a newer release).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    #[must_use]
    pub const fn spec(self) -> JobSpec {
        match self {
            Self::Housekeeping => JobSpec {
                timeout: Duration::from_mins(5),
                max_attempts: 3,
                backoff: Duration::from_mins(1),
                schedule: Some(Duration::from_hours(1)),
            },
            Self::Noop => JobSpec {
                timeout: Duration::from_secs(30),
                max_attempts: 1,
                backoff: Duration::from_secs(1),
                schedule: None,
            },
            // Probe failures are part of the report, not job failures, so a
            // retry only happens for Starr-side trouble (database).
            Self::ServicesCheck => JobSpec {
                timeout: Duration::from_mins(2),
                max_attempts: 2,
                backoff: Duration::from_secs(30),
                schedule: None,
            },
            // Interactive: an admin waits for the lists, so no retry delay
            // worth waiting for; the error is stored with the lists.
            Self::SourcesLists => JobSpec {
                timeout: Duration::from_mins(1),
                max_attempts: 1,
                backoff: Duration::from_secs(5),
                schedule: None,
            },
            // Listing failures are part of the report; a retry only happens
            // for Starr-side trouble. Hourly keeps availability fresh while
            // absence still needs two complete runs and six hours.
            Self::InventorySync => JobSpec {
                timeout: Duration::from_mins(15),
                max_attempts: 2,
                backoff: Duration::from_mins(1),
                schedule: Some(Duration::from_hours(1)),
            },
            // Failures of single acquisitions are part of the report and
            // retried per acquisition (`requests::resubmit_after`); a job
            // retry only happens for Starr-side trouble. The schedule picks
            // up resubmissions that came due.
            Self::RequestsSubmit => JobSpec {
                timeout: Duration::from_mins(10),
                max_attempts: 3,
                backoff: Duration::from_mins(1),
                schedule: Some(Duration::from_mins(5)),
            },
            // "Every few minutes" (spec 0003); only while something is
            // being acquired.
            Self::AcquisitionsSync => JobSpec {
                timeout: Duration::from_mins(10),
                max_attempts: 2,
                backoff: Duration::from_mins(1),
                schedule: Some(Duration::from_mins(5)),
            },
            // Failed deliveries are part of the report and retried per
            // delivery (`notifications::retry_delay`); a job retry only
            // happens for Starr-side trouble. The requests API enqueues it
            // right after a change that left deliveries due; the minute
            // schedule covers changes made by jobs and retries.
            Self::NotificationsDeliver => JobSpec {
                timeout: Duration::from_mins(5),
                max_attempts: 2,
                backoff: Duration::from_secs(30),
                schedule: Some(Duration::from_mins(1)),
            },
            // Each follow daily (spec 0003), spread over the day (spec 0006
            // Q14): every hour for the follows due
            // (`follows::CHECK_INTERVAL`), only while some are. Catalog
            // failures of single follows are part of the report and picked
            // up by the follow's next check; a retry only happens for
            // Starr-side trouble.
            Self::FollowsCheck => JobSpec {
                timeout: Duration::from_mins(15),
                max_attempts: 2,
                backoff: Duration::from_mins(5),
                schedule: Some(Duration::from_hours(1)),
            },
            // Every 15 minutes (spec 0005). Listing failures are part of the
            // report and stored per instance; a retry only happens for
            // Starr-side trouble. "Gone" needs two complete listings.
            Self::AccountsScan => JobSpec {
                timeout: Duration::from_mins(5),
                max_attempts: 2,
                backoff: Duration::from_mins(1),
                schedule: Some(Duration::from_mins(15)),
            },
            // Plans from the database; on live instances also writes to the
            // service (a few requests per step within its rate limit), so a
            // large approved plan needs time. Triggered by scans and
            // changes, so no schedule of its own.
            Self::AccountsReconcile => JobSpec {
                timeout: Duration::from_mins(15),
                max_attempts: 2,
                backoff: Duration::from_mins(1),
                schedule: None,
            },
            // A member waits for it (just set a password). Failures of
            // single accounts are part of the report; the handoff stays
            // for a later run within its hour. A retry only happens for
            // Starr-side trouble.
            Self::AccountsPassword => JobSpec {
                timeout: Duration::from_mins(5),
                max_attempts: 2,
                backoff: Duration::from_secs(30),
                schedule: None,
            },
            // Twice a day, so lists cached for a day are replaced before
            // they expire. Failures of single lists are part of the report;
            // a retry only happens for Starr-side trouble.
            Self::CatalogRefreshLists => JobSpec {
                timeout: Duration::from_mins(10),
                max_attempts: 2,
                backoff: Duration::from_mins(5),
                schedule: Some(Duration::from_hours(12)),
            },
        }
    }

    /// Unique key that serialises all executions of this kind, whatever the
    /// trigger (lesson 20: exclusion belongs to the operation).
    #[must_use]
    pub fn unique_key(self) -> String {
        format!("kind:{}", self.name())
    }
}

/// What caused a job to be enqueued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    Schedule,
    Manual,
}

impl Trigger {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Manual => "manual",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Schedule, Self::Manual]
            .into_iter()
            .find(|t| t.as_str() == s)
    }
}

/// State of one execution of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    /// The handler is executing (the job holds a lease).
    Running,
    /// The handler returned a report.
    Succeeded,
    /// The handler failed or timed out; no report.
    Failed,
    /// Execution stopped from outside: shutdown, lost or expired lease.
    Aborted,
}

impl RunState {
    pub const ALL: [Self; 4] = [Self::Running, Self::Succeeded, Self::Failed, Self::Aborted];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Aborted => "aborted",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == s)
    }
}

/// Whether a run had the effect it should have had (lesson 28: green without
/// effect is a failure, shown as a warning).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "detail")]
pub enum Expectation {
    Met(String),
    Unmet(String),
    NotApplicable,
}

impl Expectation {
    #[must_use]
    pub const fn status(&self) -> &'static str {
        match self {
            Self::Met(_) => "met",
            Self::Unmet(_) => "unmet",
            Self::NotApplicable => "not_applicable",
        }
    }
}

/// Outcome of one unit of work inside a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemOutcome {
    Succeeded,
    Failed,
    Skipped,
}

/// One unit of work in a report, e.g. one service instance checked. `code`
/// is machine-readable (an error taxonomy code); `detail` is for operators
/// and never contains secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportItem {
    pub target: String,
    pub outcome: ItemOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Structured result of a successful run. Units of work are partitioned into
/// succeeded, failed and skipped; the constructor rejects counts that do not
/// add up to the input set (lesson 15).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    total: u64,
    succeeded: u64,
    failed: u64,
    skipped: u64,
    counters: BTreeMap<String, u64>,
    expectation: Expectation,
    /// Per-unit outcomes, where the job has distinct units.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    items: Vec<ReportItem>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "report counts do not add up: {succeeded} succeeded + {failed} failed + {skipped} skipped != {total} total"
)]
pub struct ReportError {
    pub total: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub skipped: u64,
}

impl Report {
    pub fn new(
        total: u64,
        succeeded: u64,
        failed: u64,
        skipped: u64,
        expectation: Expectation,
    ) -> Result<Self, ReportError> {
        let sum = succeeded
            .checked_add(failed)
            .and_then(|s| s.checked_add(skipped));
        if sum != Some(total) {
            return Err(ReportError {
                total,
                succeeded,
                failed,
                skipped,
            });
        }
        Ok(Self {
            total,
            succeeded,
            failed,
            skipped,
            counters: BTreeMap::new(),
            expectation,
            items: Vec::new(),
        })
    }

    /// Adds a named counter (e.g. `runs_deleted`).
    #[must_use]
    pub fn with_counter(mut self, name: &str, value: u64) -> Self {
        self.counters.insert(name.to_owned(), value);
        self
    }

    /// Records the outcome of one unit of work.
    #[must_use]
    pub fn with_item(mut self, item: ReportItem) -> Self {
        self.items.push(item);
        self
    }

    #[must_use]
    pub fn items(&self) -> &[ReportItem] {
        &self.items
    }

    #[must_use]
    pub fn expectation(&self) -> &Expectation {
        &self.expectation
    }

    #[must_use]
    pub fn counter(&self, name: &str) -> Option<u64> {
        self.counters.get(name).copied()
    }

    #[must_use]
    pub fn failed(&self) -> u64 {
        self.failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_are_unique() {
        for kind in JobKind::ALL {
            assert_eq!(JobKind::from_name(kind.name()), Some(kind));
        }
        assert_eq!(JobKind::from_name("system.unknown"), None);
    }

    #[test]
    fn specs_are_sane() {
        for kind in JobKind::ALL {
            let spec = kind.spec();
            assert!(spec.max_attempts >= 1, "{kind:?}");
            assert!(!spec.timeout.is_zero(), "{kind:?}");
        }
        assert_eq!(
            JobKind::Housekeeping.spec().schedule,
            Some(Duration::from_hours(1))
        );
    }

    #[test]
    fn report_counts_must_add_up() {
        assert!(Report::new(3, 1, 1, 1, Expectation::NotApplicable).is_ok());
        let err = Report::new(3, 1, 1, 0, Expectation::NotApplicable).unwrap_err();
        assert_eq!(err.total, 3);
        assert!(Report::new(u64::MAX, u64::MAX, 1, 0, Expectation::NotApplicable).is_err());
    }

    #[test]
    fn states_round_trip() {
        for state in RunState::ALL {
            assert_eq!(RunState::parse(state.as_str()), Some(state));
        }
        assert_eq!(Trigger::parse("manual"), Some(Trigger::Manual));
        assert_eq!(Trigger::parse("cron"), None);
    }
}
