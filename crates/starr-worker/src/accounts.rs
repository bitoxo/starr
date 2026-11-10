//! `accounts.scan` (spec 0005 D7, slices 1 and 2): read the libraries and
//! accounts of every consume instance whose adapter implements [`Accounts`]
//! (Jellyfin, Emby, Navidrome) into the library list (what the admin maps to
//! categories) and the foreign account inventory. Reads only: the adapters
//! have no write method yet, and the scan calls nothing but `probe`,
//! `libraries` and `accounts`. A failed library listing is recorded and
//! does not stop the account listing.
//!
//! "Gone" needs two complete listings in a row
//! (`starr_core::accounts::presence`); a failed or incomplete listing never
//! makes an account go.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use starr_core::accounts::{self, Accounts, KnownAccount, LibraryGrant, Presence, RemoteAccount};
use starr_core::adapters::{AdapterError, Listing};
use starr_core::jobs::{Expectation, ItemOutcome, Report, ReportItem, Trigger};
use starr_core::library::{Library, MediaLibrary};
use starr_db::Database;
use starr_db::remote_accounts::{AccountScan, MissingAccount, ObservedAccount, RemoteAccountRow};

use crate::services::{Failure, Openers, instance_client};
use crate::{JobContext, JobError, adapters};

/// What one instance listed.
struct Read {
    libraries: Result<Listing<MediaLibrary>, AdapterError>,
    accounts: Result<Listing<RemoteAccount>, AdapterError>,
}

/// Probe first (product and credential; a version outside the tested range
/// still allows reading, ADR 0009), then list libraries and accounts.
async fn read<A: Accounts + Library>(adapter: &A) -> Result<Read, AdapterError> {
    match adapter.probe().await {
        Ok(_) | Err(AdapterError::VersionUnsupported { .. }) => {}
        Err(err) => return Err(err),
    }
    let libraries = adapter.libraries().await;
    let accounts = adapter.accounts().await;
    Ok(Read {
        libraries,
        accounts,
    })
}

pub(crate) async fn scan(ctx: &JobContext, openers: Openers<'_>) -> Result<Report, JobError> {
    // Every consume instance; those without accounts are skipped.
    let ids = ctx.db.consume_instance_ids().await?;
    let mut items = Vec::with_capacity(ids.len());
    let mut totals = Counts::default();
    let (mut ok, mut failed, mut skipped, mut incomplete) = (0u64, 0u64, 0u64, 0u64);
    for id in ids {
        if ctx.cancel.is_cancelled() {
            return Err(JobError::Cancelled);
        }
        let outcome = tokio::select! {
            outcome = scan_one(&ctx.db, openers, id, Utc::now()) => outcome?,
            () = ctx.cancel.cancelled() => return Err(JobError::Cancelled),
        };
        match &outcome {
            Scanned::Done { counts, .. } => {
                ok += 1;
                if !counts.complete {
                    incomplete += 1;
                }
                totals.add(counts);
            }
            Scanned::Failed { .. } => failed += 1,
            Scanned::Skipped { .. } => skipped += 1,
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
    for (name, value) in totals.named() {
        report = report.with_counter(name, value);
    }
    for item in items {
        report = report.with_item(item);
    }
    // Plan again with what this scan observed, also after a failure (the
    // plan is then marked incomplete). The planner never decides absence or
    // creates from an incomplete listing.
    if ok + failed > 0 {
        crate::provisioning::enqueue_reconcile(&ctx.db, Trigger::Schedule).await?;
    }
    Ok(report)
}

/// How one instance's scan ended.
enum Scanned {
    Done {
        target: String,
        counts: Counts,
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

impl Scanned {
    fn into_item(self) -> ReportItem {
        match self {
            Self::Done { target, counts } => {
                let mut detail: Vec<String> = counts
                    .named()
                    .iter()
                    .map(|(k, v)| format!("{k} {v}"))
                    .collect();
                if !counts.complete {
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

async fn scan_one(
    db: &Database,
    openers: Openers<'_>,
    id: i64,
    now: DateTime<Utc>,
) -> Result<Scanned, JobError> {
    let Some(conn) = db.service_connection(id).await? else {
        return Ok(Scanned::Skipped {
            target: format!("service {id}"),
            reason: "removed before the scan ran",
        });
    };
    let target = format!("{} ({})", conn.name, conn.adapter_type);
    if !adapters::has_accounts(&conn.adapter_type) {
        return Ok(Scanned::Skipped {
            target,
            reason: "this service type has no accounts to read",
        });
    }
    let client = match instance_client(openers, &conn) {
        Ok((_, client)) => client,
        Err(failure) => {
            db.record_account_scan_failure(id, failure.code, now)
                .await?;
            db.record_library_listing_failure(id, failure.code, now)
                .await?;
            return Ok(Scanned::Failed { target, failure });
        }
    };
    let result = match conn.adapter_type.as_str() {
        "jellyfin" => read(&starr_adapter_jellyfin::Jellyfin::new(client)).await,
        "emby" => read(&starr_adapter_emby::Emby::new(client)).await,
        "navidrome" => read(&starr_adapter_navidrome::Navidrome::new(client)).await,
        _ => {
            return Ok(Scanned::Skipped {
                target,
                reason: "this service type has no accounts to read",
            });
        }
    };
    let (libraries, accounts) = match result {
        Ok(read) => (Some(read.libraries), read.accounts),
        Err(err) => (None, Err(err)),
    };
    // Without a probe nothing was listed: both listings failed alike.
    let probe_failed = libraries.is_none();
    let mut library_count = 0;
    match libraries {
        Some(Ok(libraries)) => {
            library_count = libraries.items.len() as u64;
            db.store_library_listing(id, &libraries, now).await?;
        }
        Some(Err(err)) => {
            tracing::warn!(service = id, code = err.code(), error = %err, "library listing failed");
            db.record_library_listing_failure(id, err.code(), now)
                .await?;
        }
        None => {}
    }
    let listing = match accounts {
        Ok(listing) => listing,
        Err(err) => {
            tracing::warn!(service = id, code = err.code(), error = %err, "account listing failed");
            db.record_account_scan_failure(id, err.code(), now).await?;
            if probe_failed {
                db.record_library_listing_failure(id, err.code(), now)
                    .await?;
            }
            return Ok(Scanned::Failed {
                target,
                failure: err.into(),
            });
        }
    };

    let previous = db.remote_accounts(id).await?;
    let (scan, mut counts) = plan(&previous, &listing);
    counts.libraries = library_count;
    if !db.store_account_scan(id, &scan, now).await? {
        return Ok(Scanned::Skipped {
            target,
            reason: "removed while the scan ran",
        });
    }
    tracing::info!(
        service = id,
        complete = counts.complete,
        listed = counts.listed,
        "accounts scanned"
    );
    Ok(Scanned::Done { target, counts })
}

/// Counters of one instance's scan.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Counts {
    complete: bool,
    listed: u64,
    privileged: u64,
    appeared: u64,
    returned: u64,
    missing: u64,
    gone: u64,
    /// Libraries listed.
    libraries: u64,
}

impl Counts {
    fn named(&self) -> [(&'static str, u64); 7] {
        [
            ("libraries", self.libraries),
            ("listed", self.listed),
            ("privileged", self.privileged),
            ("appeared", self.appeared),
            ("returned", self.returned),
            ("missing", self.missing),
            ("gone", self.gone),
        ]
    }

    fn add(&mut self, other: &Self) {
        self.listed += other.listed;
        self.privileged += other.privileged;
        self.appeared += other.appeared;
        self.returned += other.returned;
        self.missing += other.missing;
        self.gone += other.gone;
        self.libraries += other.libraries;
    }
}

/// Turn a listing into what to store, with the pure presence decisions.
fn plan(previous: &[RemoteAccountRow], listing: &Listing<RemoteAccount>) -> (AccountScan, Counts) {
    let known: Vec<KnownAccount> = previous
        .iter()
        .map(|row| KnownAccount {
            remote_id: row.remote_id.clone(),
            missing_runs: row.missing_runs,
            gone: row.gone_since.is_some(),
        })
        .collect();
    let by_id: HashMap<&str, &RemoteAccount> = listing
        .items
        .iter()
        .map(|a| (a.remote_id.as_str(), a))
        .collect();
    let mut counts = Counts {
        complete: listing.complete,
        ..Counts::default()
    };
    let mut scan = AccountScan {
        complete: listing.complete,
        observed: Vec::new(),
        missing: Vec::new(),
    };
    for (remote_id, presence) in accounts::presence(&known, listing) {
        match presence {
            Presence::Appeared | Presence::Returned | Presence::Present => {
                if presence == Presence::Appeared {
                    counts.appeared += 1;
                } else if presence == Presence::Returned {
                    counts.returned += 1;
                }
                let Some(account) = by_id.get(remote_id.as_str()) else {
                    continue;
                };
                counts.listed += 1;
                counts.privileged += u64::from(account.privileged);
                scan.observed.push(ObservedAccount {
                    remote_id,
                    username: account.username.clone(),
                    enabled: account.enabled,
                    privileged: account.privileged,
                    locked_by_service: account.locked_by_service,
                    libraries: match &account.libraries {
                        LibraryGrant::All => None,
                        LibraryGrant::Only(ids) => Some(ids.clone()),
                    },
                });
            }
            Presence::Missing { missing_runs } => {
                counts.missing += 1;
                scan.missing.push(MissingAccount {
                    remote_id,
                    missing_runs,
                    gone: false,
                });
            }
            Presence::Gone { missing_runs } => {
                counts.gone += 1;
                scan.missing.push(MissingAccount {
                    remote_id,
                    missing_runs,
                    gone: true,
                });
            }
        }
    }
    (scan, counts)
}
