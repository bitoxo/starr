//! Catalog access (spec 0003, 0006): the catalog adapter of a service
//! instance (TMDB or `MusicBrainz`) over the instance's shared client, with
//! responses cached in the `catalog_cache` table.
//!
//! Catalog read tokens are sealed to the web-held key (ADR 0003 amendment
//! 1), so [`CatalogAccess`] works in the web process (interactive search,
//! discover, the relevance preview) as well as in the worker (checks,
//! catalog jobs). It opens catalog tokens only: an instance whose adapter is
//! not a catalog, or whose credential is a service credential, is refused
//! before anything is decrypted.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::Utc;
use secrecy::SecretString;
use starr_adapter_kit::{BoxFuture, ClientPool, ResponseCache, ServiceClient};
use starr_adapter_musicbrainz::{GENRES, MusicBrainz};
use starr_adapter_stashdb::StashDb;
use starr_adapter_tmdb::Tmdb;
use starr_core::adapters::{AdapterDescriptor, AdapterError, Probe, ProbeReport, SecretClass};
use starr_core::catalog::{
    Catalog, CatalogConfiguration, CatalogDetails, CatalogEntry, CatalogRef, ListQuery, MediaKind,
    Page, SearchQuery, WatchProvider,
};
use starr_core::jobs::{Expectation, ItemOutcome, Report, ReportItem};
use starr_core::members::Category;
use starr_db::services::ServiceConnection;
use starr_db::{Database, DbError};
use starr_secrets::CatalogTokenKey;
use url::Url;

use crate::services::{CODE_CREDENTIALS_UNREADABLE, Failure, build_client};
use crate::{JobContext, JobError, SystemJobs};

/// Rows kept in `catalog_cache` at most; housekeeping deletes the
/// soonest-expiring beyond it. Lists and details are a few KB each.
pub const CATALOG_CACHE_MAX_ROWS: u32 = 20_000;

/// Error code of a catalog instance the process cannot open because it has
/// no web key (a split-mode worker without `web.key` mounted).
pub const CODE_CATALOG_KEY_UNAVAILABLE: &str = "catalog_key_unavailable";

/// `app_meta` key holding the recipient (public key) catalog tokens are
/// sealed for: the web key's. Published by the web side; the worker reads
/// it to re-seal TMDB tokens it still holds under its own key.
pub const CATALOG_RECIPIENT_META_KEY: &str = "catalog_tokens_recipient";

/// Response cache of one service instance in `catalog_cache`. Best effort:
/// database errors are logged and count as a miss.
#[derive(Debug)]
pub struct DbResponseCache {
    db: Database,
    instance_id: i64,
}

impl DbResponseCache {
    #[must_use]
    pub fn new(db: Database, instance_id: i64) -> Self {
        Self { db, instance_id }
    }
}

impl ResponseCache for DbResponseCache {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            match self
                .db
                .catalog_cache_get(self.instance_id, key, Utc::now())
                .await
            {
                Ok(hit) => hit,
                Err(err) => {
                    tracing::warn!(service = self.instance_id, error = %err, "catalog cache read failed");
                    None
                }
            }
        })
    }

    fn put<'a>(&'a self, key: &'a str, body: &'a [u8], ttl: Duration) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Err(err) = self
                .db
                .catalog_cache_put(self.instance_id, key, body, Utc::now(), ttl)
                .await
            {
                tracing::warn!(service = self.instance_id, error = %err, "catalog cache write failed");
            }
        })
    }
}

/// Why no catalog could be opened for an instance.
#[derive(Debug, thiserror::Error)]
pub enum CatalogOpenError {
    #[error("service {0} does not exist")]
    Missing(i64),
    #[error("service {id} is a {adapter_type}, not a catalog")]
    NotACatalog { id: i64, adapter_type: String },
    /// The client could not be built (unknown adapter, unreadable or not
    /// yet re-sealed credential, no web key, invalid URL); `code` as in
    /// `services.check`.
    #[error("{detail}")]
    Unavailable { code: &'static str, detail: String },
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<Failure> for CatalogOpenError {
    fn from(f: Failure) -> Self {
        Self::Unavailable {
            code: f.code,
            detail: f.detail,
        }
    }
}

/// A catalog adapter of one of the known types. [`Catalog`] is not object
/// safe (its methods return `impl Future`), so callers get this enum.
#[derive(Debug, Clone)]
pub enum AnyCatalog {
    Tmdb(Tmdb),
    MusicBrainz(MusicBrainz),
    /// The private category's catalog (spec 0007): only ever opened by
    /// code paths behind the privacy gate.
    StashDb(StashDb),
}

impl Probe for AnyCatalog {
    fn descriptor(&self) -> &'static AdapterDescriptor {
        match self {
            Self::Tmdb(c) => c.descriptor(),
            Self::MusicBrainz(c) => c.descriptor(),
            Self::StashDb(c) => c.descriptor(),
        }
    }

    async fn probe(&self) -> Result<ProbeReport, AdapterError> {
        match self {
            Self::Tmdb(c) => c.probe().await,
            Self::MusicBrainz(c) => c.probe().await,
            Self::StashDb(c) => c.probe().await,
        }
    }
}

impl Catalog for AnyCatalog {
    async fn configuration(&self) -> Result<CatalogConfiguration, AdapterError> {
        match self {
            Self::Tmdb(c) => c.configuration().await,
            Self::MusicBrainz(c) => c.configuration().await,
            Self::StashDb(c) => c.configuration().await,
        }
    }

    async fn providers(
        &self,
        kind: MediaKind,
        region: &str,
    ) -> Result<Vec<WatchProvider>, AdapterError> {
        match self {
            Self::Tmdb(c) => c.providers(kind, region).await,
            Self::MusicBrainz(c) => c.providers(kind, region).await,
            Self::StashDb(c) => c.providers(kind, region).await,
        }
    }

    async fn search(&self, query: &SearchQuery<'_>) -> Result<Page<CatalogEntry>, AdapterError> {
        match self {
            Self::Tmdb(c) => c.search(query).await,
            Self::MusicBrainz(c) => c.search(query).await,
            Self::StashDb(c) => c.search(query).await,
        }
    }

    async fn list(&self, query: &ListQuery<'_>) -> Result<Page<CatalogEntry>, AdapterError> {
        match self {
            Self::Tmdb(c) => c.list(query).await,
            Self::MusicBrainz(c) => c.list(query).await,
            Self::StashDb(c) => c.list(query).await,
        }
    }

    async fn details(
        &self,
        item: &CatalogRef,
        language: &str,
    ) -> Result<CatalogDetails, AdapterError> {
        match self {
            Self::Tmdb(c) => c.details(item, language).await,
            Self::MusicBrainz(c) => c.details(item, language).await,
            Self::StashDb(c) => c.details(item, language).await,
        }
    }
}

/// Opens catalog instances with the web key. One per process; the HTTP
/// side and the in-process worker share it, so each instance has one rate
/// limiter per process, and `ListenBrainz` one per process.
#[derive(Debug)]
pub struct CatalogAccess {
    key: CatalogTokenKey,
    clients: ClientPool,
    /// This process's share of per-host budgets in percent (spec 0006
    /// Q14): 100 in single-process mode, web 70 and worker 30 when split.
    percent: u32,
    listenbrainz_base: String,
    listenbrainz: Mutex<Option<ServiceClient>>,
}

/// The web process's share of the `MusicBrainz` budget in split mode.
pub const WEB_SHARE: u32 = 70;
/// A single process (`all` mode) holds the whole budget.
pub use crate::services::FULL_SHARE;
/// The worker process's share of the `MusicBrainz` budget in split mode.
pub const WORKER_SHARE: u32 = 30;

impl CatalogAccess {
    /// Access holding the whole budget, reading lists from the public
    /// `ListenBrainz`.
    #[must_use]
    pub fn new(key: CatalogTokenKey) -> Self {
        Self {
            key,
            clients: ClientPool::new(),
            percent: FULL_SHARE,
            listenbrainz_base: starr_adapter_musicbrainz::LISTENBRAINZ_BASE.to_owned(),
            listenbrainz: Mutex::new(None),
        }
    }

    /// This process holds `percent` of each per-host budget (split mode).
    #[must_use]
    pub fn with_budget_share(mut self, percent: u32) -> Self {
        self.percent = percent;
        self
    }

    /// Read `ListenBrainz` lists from `base` instead (tests and
    /// development fakes).
    #[must_use]
    pub fn with_listenbrainz_base(mut self, base: &Url) -> Self {
        base.as_str().clone_into(&mut self.listenbrainz_base);
        self
    }

    /// The process's `ListenBrainz` client, built once.
    fn listenbrainz(&self) -> Result<ServiceClient, Failure> {
        let mut client = self
            .listenbrainz
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(client) = client.as_ref() {
            return Ok(client.clone());
        }
        let base = Url::parse(&self.listenbrainz_base).map_err(|err| {
            Failure::from(AdapterError::Unknown(format!(
                "invalid ListenBrainz base URL: {err}"
            )))
        })?;
        let built = starr_adapter_musicbrainz::listenbrainz_client(base, self.percent)?;
        *client = Some(built.clone());
        Ok(built)
    }

    /// The recipient catalog tokens are sealed for.
    #[must_use]
    pub fn recipient(&self) -> &str {
        self.key.recipient()
    }

    /// Seals catalog tokens for this key.
    #[must_use]
    pub fn sealer(&self) -> &starr_secrets::CatalogTokenSealer {
        self.key.sealer()
    }

    /// Publish this key's recipient for the worker (which re-seals with
    /// it) and mark catalog tokens sealed for another web key as
    /// unreadable (a lost or replaced `web.key`). Called by the web side
    /// on start.
    pub async fn publish(&self, db: &Database) -> Result<(), DbError> {
        let lost = db
            .mark_credentials_unreadable(
                SecretClass::CatalogReadToken,
                self.recipient(),
                CODE_CREDENTIALS_UNREADABLE,
                Utc::now(),
            )
            .await?;
        if lost > 0 {
            tracing::error!(
                services = lost,
                "web.key does not match the key stored catalog tokens were sealed with; re-enter the TMDB token of those services, or restore web.key from the backup"
            );
        }
        db.meta_set(CATALOG_RECIPIENT_META_KEY, self.recipient())
            .await
    }

    /// Open the token of a catalog connection. Refuses (without
    /// decrypting) anything that is not a catalog token.
    pub(crate) fn open(&self, conn: &ServiceConnection) -> Result<SecretString, Failure> {
        let adapter_is_catalog = crate::adapters::descriptor(&conn.adapter_type)
            .is_some_and(|d| d.secret_class == SecretClass::CatalogReadToken);
        if !adapter_is_catalog || conn.credential.class != SecretClass::CatalogReadToken {
            return Err(Failure::starr(
                CODE_CREDENTIALS_UNREADABLE,
                "the stored credential is not a catalog token (not re-sealed yet?)".to_owned(),
            ));
        }
        self.key
            .open_token(&conn.credential.recipient, &conn.credential.ciphertext)
            .map_err(|err| {
                Failure::starr(
                    CODE_CREDENTIALS_UNREADABLE,
                    format!("the stored catalog token cannot be opened: {err}"),
                )
            })
    }

    /// The shared client of a catalog connection.
    pub(crate) fn client(
        &self,
        conn: &ServiceConnection,
    ) -> Result<(&'static AdapterDescriptor, ServiceClient), Failure> {
        build_client(&self.clients, conn, self.percent, || self.open(conn))
    }

    /// Check a `StashDB` key before it is stored (spec 0007 Q14 "checked on
    /// save"): a throwaway client over `base_url` with this process's
    /// share of the budget, asking for version and roles only.
    pub async fn check_stashdb_key(
        &self,
        base_url: Url,
        key: SecretString,
    ) -> Result<starr_adapter_stashdb::KeyCheck, AdapterError> {
        let descriptor = &starr_adapter_stashdb::DESCRIPTOR;
        let client = ServiceClient::with_options(
            descriptor,
            starr_adapter_kit::Endpoint {
                base_url,
                auth: starr_adapter_stashdb::AUTH,
                credential: key,
            },
            starr_adapter_stashdb::client_options(self.percent),
        )?;
        StashDb::new(client).check_key().await
    }

    /// The catalog adapter of service instance `instance_id`, caching
    /// responses in the database.
    pub async fn catalog(
        &self,
        db: &Database,
        instance_id: i64,
    ) -> Result<AnyCatalog, CatalogOpenError> {
        let conn = db
            .service_connection(instance_id)
            .await?
            .ok_or(CatalogOpenError::Missing(instance_id))?;
        let is_tmdb = conn.adapter_type == starr_adapter_tmdb::DESCRIPTOR.type_id;
        let is_musicbrainz = conn.adapter_type == starr_adapter_musicbrainz::DESCRIPTOR.type_id;
        let is_stashdb = conn.adapter_type == starr_adapter_stashdb::DESCRIPTOR.type_id;
        if !is_tmdb && !is_musicbrainz && !is_stashdb {
            return Err(CatalogOpenError::NotACatalog {
                id: instance_id,
                adapter_type: conn.adapter_type,
            });
        }
        let (_, client) = self.client(&conn)?;
        let cache = Arc::new(DbResponseCache::new(db.clone(), instance_id));
        Ok(if is_tmdb {
            AnyCatalog::Tmdb(Tmdb::new(client).with_cache(cache))
        } else if is_stashdb {
            AnyCatalog::StashDb(StashDb::new(client).with_cache(cache))
        } else {
            AnyCatalog::MusicBrainz(
                MusicBrainz::new(client)
                    .with_listenbrainz(self.listenbrainz()?)
                    .with_cache(cache),
            )
        })
    }
}

/// The music category's default catalog when it is a `MusicBrainz` whose
/// genre lanes are worth refreshing: the category is switched on.
pub(crate) async fn music_lanes_catalog(db: &Database) -> Result<Option<i64>, DbError> {
    if !db.category_state(Category::Music).await?.enabled {
        return Ok(None);
    }
    Ok(db
        .default_catalog(Category::Music)
        .await?
        .filter(|role| role.adapter_type == starr_adapter_musicbrainz::DESCRIPTOR.type_id)
        .map(|role| role.service_instance_id))
}

/// `catalog.refresh-lists` (spec 0008 slice 2): refreshes the music genre
/// lanes, one `MusicBrainz` search per genre of [`GENRES`] at this
/// process's (the worker's) share of the budget, each cached a day. The
/// web process reads them from the cache only. Failures of single lanes
/// are part of the report; once `MusicBrainz` asks to slow down, the rest
/// wait for the next run.
pub(crate) async fn refresh_lists(ctx: &JobContext, jobs: &SystemJobs) -> Result<Report, JobError> {
    let total = GENRES.len() as u64;
    let Some(instance) = music_lanes_catalog(&ctx.db).await? else {
        return Ok(Report::new(total, 0, 0, total, Expectation::NotApplicable)?);
    };
    let mb = match jobs.catalog(&ctx.db, instance).await {
        Ok(AnyCatalog::MusicBrainz(mb)) => mb,
        Ok(_) => {
            return Ok(Report::new(total, 0, 0, total, Expectation::NotApplicable)?);
        }
        Err(CatalogOpenError::Db(err)) => return Err(err.into()),
        Err(err) => {
            let detail = err.to_string();
            let report = Report::new(total, 0, total, 0, Expectation::Unmet(detail.clone()))?;
            return Ok(report.with_item(ReportItem {
                target: format!("service #{instance}"),
                outcome: ItemOutcome::Failed,
                code: Some(open_error_code(&err).to_owned()),
                detail: Some(detail),
            }));
        }
    };
    let (mut succeeded, mut failed, mut skipped) = (0, 0, 0);
    let mut items = Vec::new();
    let mut throttled = false;
    for (genre, _) in GENRES {
        if ctx.cancel.is_cancelled() {
            return Err(JobError::Cancelled);
        }
        let target = format!("genre lane {genre}");
        if throttled {
            skipped += 1;
            items.push(ReportItem {
                target,
                outcome: ItemOutcome::Skipped,
                code: Some("throttled".to_owned()),
                detail: None,
            });
            continue;
        }
        match mb.refresh_genre_lane(genre).await {
            Ok(page) => {
                succeeded += 1;
                items.push(ReportItem {
                    target,
                    outcome: ItemOutcome::Succeeded,
                    code: None,
                    detail: Some(format!("{} albums", page.items.len())),
                });
            }
            Err(err) => {
                throttled = matches!(err, AdapterError::Throttled { .. });
                failed += 1;
                items.push(ReportItem {
                    target,
                    outcome: ItemOutcome::Failed,
                    code: Some(err.code().to_owned()),
                    detail: Some(err.to_string()),
                });
            }
        }
    }
    let expectation = if failed + skipped == 0 {
        Expectation::Met(format!("{succeeded} genre lanes refreshed"))
    } else {
        Expectation::Unmet(format!(
            "{succeeded} of {total} genre lanes refreshed; the others stay as cached until the next run"
        ))
    };
    let mut report = Report::new(total, succeeded, failed, skipped, expectation)?;
    for item in items {
        report = report.with_item(item);
    }
    Ok(report)
}

fn open_error_code(err: &CatalogOpenError) -> &'static str {
    match err {
        CatalogOpenError::Unavailable { code, .. } => code,
        CatalogOpenError::Missing(_) | CatalogOpenError::NotACatalog { .. } => "not_a_catalog",
        CatalogOpenError::Db(_) => "database",
    }
}

impl SystemJobs {
    /// The catalog adapter of service instance `instance_id` for jobs.
    /// Needs the web key ([`SystemJobs::with_catalog`]); a split-mode
    /// worker without it gets [`CODE_CATALOG_KEY_UNAVAILABLE`].
    pub async fn catalog(
        &self,
        db: &Database,
        instance_id: i64,
    ) -> Result<AnyCatalog, CatalogOpenError> {
        match &self.catalog {
            Some(access) => access.catalog(db, instance_id).await,
            None => Err(CatalogOpenError::Unavailable {
                code: CODE_CATALOG_KEY_UNAVAILABLE,
                detail: "this worker has no web key to open catalog tokens".to_owned(),
            }),
        }
    }
}
