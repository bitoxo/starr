//! The production job handlers: one function per [`JobKind`].

use std::sync::Arc;

use chrono::{TimeDelta, Utc};
use starr_adapter_kit::ClientPool;
use starr_core::jobs::{Expectation, JobKind, Report};
use starr_db::audit::{ActorKind, AuditEntry};
use starr_db::{Database, DbError};
use url::Url;

use crate::catalog::{CATALOG_CACHE_MAX_ROWS, CatalogAccess};
use crate::image_cache::{
    ADULT_IMAGE_CACHE_DEFAULT_BYTES, ADULT_UNUSED_FOR, IMAGE_CACHE_DEFAULT_BYTES, ImageCache,
};
use crate::secrets::CredentialKey;
use crate::services::Openers;
use crate::{
    JobContext, JobError, JobHandler, accounts, follows, inventory, notifications, passwords,
    provisioning, requests, services, sources,
};

/// Finished runs older than this are deleted by `system.housekeeping`.
const RUN_RETENTION: TimeDelta = TimeDelta::days(30);

/// Notifications older than this are deleted by `system.housekeeping`.
const NOTIFICATION_RETENTION: TimeDelta = TimeDelta::days(180);

/// Notifications of the private category go earlier (spec 0007 Q13): what
/// is only a message does not linger.
const PRIVATE_NOTIFICATION_RETENTION: TimeDelta = TimeDelta::days(90);

/// Dispatches every [`JobKind`] to its handler. Holds the service
/// credential key and one HTTP client per service instance, so every job
/// that talks to an instance shares its rate limiter.
#[derive(Debug)]
pub struct SystemJobs {
    pub(crate) credentials: CredentialKey,
    pub(crate) clients: ClientPool,
    /// Opens catalog tokens with the web key (ADR 0003 amendment 1);
    /// shared with the HTTP side in single-process mode. `None` in a
    /// split-mode worker without `web.key`.
    pub(crate) catalog: Option<Arc<CatalogAccess>>,
    /// The image proxy's disk cache and its size limit, kept by
    /// housekeeping.
    image_cache: Option<(ImageCache, u64)>,
    /// Size limit of the adult image cache next to it
    /// (`STARR_ADULT_IMAGE_CACHE_MB`, spec 0007 Q6).
    adult_image_bytes: u64,
    /// `STARR_PUBLIC_URL`, for links in notifications.
    public_url: Option<Url>,
}

impl SystemJobs {
    #[must_use]
    pub fn new(credentials: CredentialKey) -> Self {
        Self {
            credentials,
            clients: ClientPool::new(),
            catalog: None,
            image_cache: None,
            adult_image_bytes: ADULT_IMAGE_CACHE_DEFAULT_BYTES,
            public_url: None,
        }
    }

    /// Let housekeeping evict from the image proxy's disk cache, keeping
    /// it at most `max_bytes` (`STARR_IMAGE_CACHE_MB`).
    #[must_use]
    pub fn with_image_cache(mut self, cache: ImageCache, max_bytes: u64) -> Self {
        self.image_cache = Some((cache, max_bytes));
        self
    }

    /// Keep the adult image cache at most `max_bytes`
    /// (`STARR_ADULT_IMAGE_CACHE_MB`).
    #[must_use]
    pub fn with_adult_image_limit(mut self, max_bytes: u64) -> Self {
        self.adult_image_bytes = max_bytes;
        self
    }

    /// Link notifications to `url` (`STARR_PUBLIC_URL`).
    #[must_use]
    pub fn with_public_url(mut self, url: Option<Url>) -> Self {
        self.public_url = url;
        self
    }

    /// Open catalog tokens through `access` (the web key).
    #[must_use]
    pub fn with_catalog(mut self, access: Arc<CatalogAccess>) -> Self {
        self.catalog = Some(access);
        self
    }

    pub(crate) fn openers(&self) -> Openers<'_> {
        Openers {
            credentials: &self.credentials,
            clients: &self.clients,
            catalog: self.catalog.as_deref(),
        }
    }
}

/// The default image cache limit when none is configured (1 GiB).
pub const DEFAULT_IMAGE_CACHE_BYTES: u64 = IMAGE_CACHE_DEFAULT_BYTES;

impl JobHandler for SystemJobs {
    async fn run(&self, kind: JobKind, ctx: JobContext) -> Result<Report, JobError> {
        match kind {
            JobKind::Housekeeping => {
                let report =
                    housekeeping(&ctx, self.image_cache.as_ref(), self.adult_image_bytes).await?;
                // The re-seal is a one-time migration; its trouble must not
                // stop retention and pruning.
                let resealed = match self.credentials.reseal_catalog_tokens(&ctx.db).await {
                    Ok(r) => r.resealed as u64,
                    Err(err) => {
                        tracing::error!(error = %err, "re-sealing catalog tokens failed");
                        0
                    }
                };
                Ok(report.with_counter("catalog_tokens_resealed", resealed))
            }
            JobKind::Noop => noop(),
            JobKind::ServicesCheck => services::check(&ctx, self.openers()).await,
            JobKind::SourcesLists => sources::lists(&ctx, self.openers()).await,
            JobKind::InventorySync => inventory::sync(&ctx, self.openers()).await,
            JobKind::RequestsSubmit => requests::submit(&ctx, self.openers()).await,
            JobKind::AcquisitionsSync => requests::sync(&ctx, self.openers()).await,
            JobKind::NotificationsDeliver => {
                notifications::deliver(&ctx, &self.credentials, self.public_url.as_ref()).await
            }
            JobKind::FollowsCheck => follows::check(&ctx, self).await,
            JobKind::AccountsScan => accounts::scan(&ctx, self.openers()).await,
            JobKind::AccountsReconcile => provisioning::reconcile(&ctx, self.openers()).await,
            JobKind::AccountsPassword => {
                passwords::run(&ctx, self.openers(), &self.credentials).await
            }
            JobKind::CatalogRefreshLists => crate::catalog::refresh_lists(&ctx, self).await,
        }
    }

    /// Inventory sync is only scheduled while some category watches in a
    /// consume instance; submission, sync and the follow check while they
    /// have something to do.
    async fn has_work(&self, kind: JobKind, db: &Database) -> Result<bool, DbError> {
        match kind {
            JobKind::InventorySync => Ok(!db.consume_instance_ids().await?.is_empty()),
            JobKind::RequestsSubmit => {
                // Writes of the private category wait while it is off.
                let private_off = !db
                    .category_state(starr_core::members::Category::Adult)
                    .await?
                    .enabled_with_statement();
                db.has_due_submissions_outside(
                    Utc::now(),
                    private_off.then_some(starr_core::members::Category::Adult),
                )
                .await
            }
            JobKind::AcquisitionsSync => Ok(db.has_synced_acquisitions().await?
                || db.has_removal_candidates(Utc::now()).await?
                || db.has_scene_file_checks(Utc::now()).await?),
            JobKind::NotificationsDeliver => db.has_due_deliveries(Utc::now()).await,
            JobKind::FollowsCheck => {
                db.has_due_follows(Utc::now() - starr_core::follows::CHECK_INTERVAL)
                    .await
            }
            JobKind::AccountsScan | JobKind::AccountsReconcile => has_account_instances(db).await,
            // Only while music is on with a MusicBrainz catalog.
            JobKind::CatalogRefreshLists => {
                Ok(crate::catalog::music_lanes_catalog(db).await?.is_some())
            }
            _ => Ok(true),
        }
    }

    /// The active worker publishes its credential recipient (ADR 0003) and
    /// re-seals catalog tokens still held under its key (amendment 1).
    async fn on_lease_acquired(&self, db: &Database) -> Result<(), DbError> {
        self.credentials.publish(db).await?;
        self.credentials.reseal_catalog_tokens(db).await?;
        Ok(())
    }
}

/// Whether some consume instance's adapter reads accounts (`accounts.scan`
/// has work).
async fn has_account_instances(db: &Database) -> Result<bool, DbError> {
    for id in db.consume_instance_ids().await? {
        if let Some(service) = db.get_service(id).await?
            && crate::adapters::has_accounts(&service.adapter_type)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Deletes old runs and notifications (the private category's after 90
/// days), turns members whose deletion grace period ended into
/// tombstones (domain model §1), and keeps the catalog response cache and
/// the image caches in bounds: adult images not used for 30 days go, and
/// while the adult category is off its images and cached catalog answers
/// go entirely (spec 0007 Q13; switching it off purges at once too).
async fn housekeeping(
    ctx: &JobContext,
    images: Option<&(ImageCache, u64)>,
    adult_image_bytes: u64,
) -> Result<Report, JobError> {
    let now = Utc::now();
    let deleted = ctx
        .db
        .delete_runs_finished_before(now - RUN_RETENTION)
        .await?;
    let tombstoned = ctx.db.tombstone_due_members(now).await?;
    let notifications = ctx
        .db
        .delete_notifications_before(now - NOTIFICATION_RETENTION)
        .await?;
    // ADR 0010: a handoff lives at most an hour, also without a worker run.
    let handoffs = ctx.db.delete_expired_password_handoffs(now).await?;
    for username in &tombstoned {
        let entry = AuditEntry {
            actor_kind: ActorKind::System,
            actor_name: None,
            action: "member.tombstoned",
            target: Some(username),
            details: None,
        };
        // The tombstone is already committed; a retry would not find it again.
        if let Err(err) = ctx.db.audit(&entry, now).await {
            tracing::error!(%username, error = %err, "member tombstoned but audit entry not written");
        }
    }
    let tombstoned = tombstoned.len() as u64;
    let cache_rows = ctx
        .db
        .catalog_cache_prune(now, CATALOG_CACHE_MAX_ROWS)
        .await?;
    let images_deleted = match images {
        Some((cache, max_bytes)) => {
            let (cache, max_bytes) = (cache.clone(), *max_bytes);
            blocking_count("image cache eviction", move || {
                cache.evict(max_bytes).map(|r| r.deleted)
            })
            .await
        }
        None => 0,
    };
    let adult_on = ctx
        .db
        .category_state(starr_core::members::Category::Adult)
        .await?
        .enabled_with_statement();
    let private_rows = if adult_on {
        0
    } else {
        ctx.db
            .catalog_cache_purge_prefix(starr_adapter_stashdb::CACHE_PREFIX)
            .await?
    };
    let private_images = match images {
        Some((cache, _)) => {
            let adult = cache.adult();
            blocking_count("adult image cache eviction", move || {
                if adult_on {
                    adult
                        .evict_unused(adult_image_bytes, ADULT_UNUSED_FOR)
                        .map(|r| r.deleted)
                } else {
                    adult.purge()
                }
            })
            .await
        }
        None => 0,
    };
    // After the cache purges, so trouble here never keeps them from running.
    let private_notifications = ctx
        .db
        .delete_category_notifications_before(
            starr_core::members::Category::Adult,
            now - PRIVATE_NOTIFICATION_RETENTION,
        )
        .await?;
    let total = deleted + tombstoned;
    Ok(Report::new(total, total, 0, 0, Expectation::NotApplicable)?
        .with_counter("runs_deleted", deleted)
        .with_counter("members_tombstoned", tombstoned)
        .with_counter("notifications_deleted", notifications)
        .with_counter("private_notifications_deleted", private_notifications)
        .with_counter("password_handoffs_expired", handoffs)
        .with_counter("catalog_cache_rows_deleted", cache_rows)
        .with_counter("images_evicted", images_deleted)
        .with_counter("private_images_deleted", private_images)
        .with_counter("private_cache_rows_deleted", private_rows))
}

/// Run a cache step off the async threads; the caches are an optimisation,
/// so trouble is reported and counts as nothing done, never a failed run.
async fn blocking_count(
    what: &'static str,
    step: impl FnOnce() -> std::io::Result<u64> + Send + 'static,
) -> u64 {
    match tokio::task::spawn_blocking(step).await {
        Ok(Ok(deleted)) => deleted,
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "{what} failed");
            0
        }
        Err(err) => {
            tracing::warn!(error = %err, "{what} panicked");
            0
        }
    }
}

fn noop() -> Result<Report, JobError> {
    Ok(Report::new(
        0,
        0,
        0,
        0,
        Expectation::Met("noop has nothing to do".to_owned()),
    )?)
}
