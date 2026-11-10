//! Starr persistence layer: a single embedded SQLite database accessed via sqlx.
//!
//! - WAL mode, foreign keys on, busy timeout set. Write transactions start
//!   with `BEGIN IMMEDIATE` via [`Database::begin_write`].
//! - Migrations live in `migrations/` and are embedded into the binary.
//! - Migrations follow ADR 0008: applied migrations unknown to this binary are
//!   ignored (rollback by image tag), and an existing database is backed up
//!   with `VACUUM INTO` before pending migrations run.
//! - Several processes may open the same file at once (split mode, ADR
//!   0003). sqlx's SQLite migrator takes no lock, so [`Database::open`]
//!   serialises the backup-and-migrate step with an exclusive lock on
//!   `<database>.migrate.lock`: the first process migrates, the others wait
//!   and then find nothing pending (no second backup).
//! - Repository modules add methods to [`Database`]: [`jobs`] (queue),
//!   [`runs`] (run history), [`auth`] (accounts, sessions, login attempts),
//!   [`audit`] (audit log), [`members`] (members, entitlements, deletion,
//!   password tokens), [`invitations`], [`admins`] (admin accounts, second
//!   factor, linked member), [`worker_lease`] (the one active worker),
//!   [`categories`] (categories, service roles), [`source_lists`] (quality
//!   profiles and root folders read from sources), [`catalog`] (catalog
//!   items, external references), [`inventory`] (what consume services
//!   have), [`requests`] (requests, acquisitions, request events),
//!   [`notifications`] (streams, channels, preferences, deliveries),
//!   [`follows`] (follows of series), [`libraries`] (libraries of consume
//!   instances).

/// SQL: the value of item `$item`'s natural key in schemes `$schemes` (an
/// SQL list), read from `catalog_key` (ADR 0012: keys are read only from
/// there, the old key columns of `catalog_item` only written).
macro_rules! key_value {
    ($item:literal, $schemes:literal) => {
        concat!(
            "(SELECT k.value FROM catalog_key k WHERE k.catalog_item_id = ",
            $item,
            " AND k.scheme IN (",
            $schemes,
            "))"
        )
    };
}

/// SQL: item `$item`'s TMDB id (movie or TV, by its kind) as an integer.
macro_rules! tmdb_key {
    ($item:literal) => {
        concat!(
            "CAST(",
            key_value!($item, "'tmdb_movie', 'tmdb_tv'"),
            " AS INTEGER)"
        )
    };
}

/// SQL: item `$item`'s TVDB series id as an integer.
macro_rules! tvdb_key {
    ($item:literal) => {
        concat!("CAST(", key_value!($item, "'tvdb'"), " AS INTEGER)")
    };
}

/// SQL: item `$item`'s `IMDb` id.
macro_rules! imdb_key {
    ($item:literal) => {
        key_value!($item, "'imdb'")
    };
}

/// SQL: item `$item`'s `MusicBrainz` artist MBID (artists).
macro_rules! mb_artist_key {
    ($item:literal) => {
        key_value!($item, "'mb_artist'")
    };
}

/// SQL: item `$item`'s `MusicBrainz` release-group MBID (albums).
macro_rules! mb_release_group_key {
    ($item:literal) => {
        key_value!($item, "'mb_release_group'")
    };
}

/// SQL: item `$item`'s `StashDB` scene UUID (scenes, spec 0007).
macro_rules! stash_scene_key {
    ($item:literal) => {
        key_value!($item, "'stashdb_scene'")
    };
}

/// SQL condition: inventory row `$item` counts for consume role `$role`
/// (spec 0005 M4, ADR 0012 known gap). A role that maps libraries counts
/// only items listed in one of them. A role without a mapping keeps the
/// behaviour from before libraries were mapped (every item of the
/// instance), except items listed only in libraries another category maps
/// there: an adult library mapped to adult never counts as movies.
macro_rules! in_role_libraries {
    ($role:literal, $item:literal) => {
        concat!(
            "(EXISTS (SELECT 1 FROM json_each(", $role, ".settings, '$.libraries') rl, json_each(",
            $item,
            ".library_ids) il WHERE il.value = json_extract(rl.value, '$.id'))
              OR (coalesce(json_array_length(", $role, ".settings, '$.libraries'), 0) = 0
                  AND NOT (coalesce(json_array_length(", $item, ".library_ids), 0) > 0
                       AND NOT EXISTS (SELECT 1 FROM json_each(", $item, ".library_ids) ol
                            WHERE NOT EXISTS (
                                SELECT 1 FROM service_role mr, json_each(mr.settings, '$.libraries') ml
                                WHERE mr.service_instance_id = ", $item, ".service_instance_id
                                  AND mr.kind = 'consume'
                                  AND json_extract(ml.value, '$.id') = ol.value)))))"
        )
    };
}

pub mod account_lifecycle;
pub mod account_writes;
pub mod admins;
pub mod audit;
pub mod auth;
pub mod catalog;
pub mod catalog_cache;
pub mod categories;
pub mod follows;
pub mod inventory;
pub mod invitations;
pub mod jobs;
pub mod libraries;
pub mod members;
pub mod notifications;
pub mod password_handoffs;
pub mod remote_accounts;
pub mod requests;
pub mod runs;
pub mod service_accounts;
pub mod services;
pub mod source_lists;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tolerant_tests;
pub mod worker_lease;

use std::ffi::OsStr;
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

/// How many pre-migration backups are kept in the backup directory.
const BACKUPS_TO_KEEP: usize = 5;

/// Errors from the persistence layer.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("database backup in {path} failed: {source}")]
    Backup {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("creating database file {path} failed: {source}")]
    CreateFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("migration lock {path} failed: {source}")]
    MigrationLock {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("backup path is not valid UTF-8: {0}")]
    NonUtf8Path(PathBuf),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid data in database: {0}")]
    InvalidData(String),
    /// A stored enum value this binary does not know, e.g. a state written
    /// by a newer release before a rollback (ADR 0008, 0012). Lists skip
    /// such rows ([`readable`]); reading one row with it fails.
    #[error("unknown {what} `{value}` in database")]
    UnknownValue { what: &'static str, value: String },
    #[error("a service with this name already exists")]
    ServiceNameTaken,
    #[error("username is already taken")]
    UsernameTaken,
    #[error("email is already used by another member")]
    EmailTaken,
    #[error("at least one active admin must remain")]
    LastActiveAdmin,
    #[error("the member is already linked to another admin")]
    MemberAlreadyLinked,
    #[error("the service instance lacks the capability this role needs")]
    RoleCapabilityMissing,
    #[error("the service instance already has this role in the category")]
    RoleTaken,
    #[error("the category already has a source")]
    SourceTaken,
    #[error("another category maps this library on the same instance")]
    LibraryTaken,
    #[error("the service instance does not exist")]
    ServiceMissing,
    #[error("the member already has an open request for this")]
    RequestOpen,
    #[error("this is available already")]
    AlreadyAvailable,
    #[error("the request belongs to another member")]
    OwnerMismatch,
    #[error("the category has no source")]
    CategorySourceMissing,
    #[error("the approval cannot be undone any more")]
    UndoTooLate,
    #[error("a notification channel with this name (or an email channel) exists already")]
    ChannelTaken,
    #[error("the follow ended")]
    FollowEnded,
}

/// Timestamps are stored as UTC ISO-8601 TEXT with millisecond precision and a
/// `Z` suffix, so that lexical comparison in SQL equals time comparison.
pub(crate) fn ts(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// The error for a stored enum value this binary does not know.
pub(crate) fn unknown(what: &'static str, value: &str) -> DbError {
    DbError::UnknownValue {
        what,
        value: value.to_owned(),
    }
}

/// Collect the rows of a list, skipping rows with a value this binary does
/// not know ([`DbError::UnknownValue`]) with a warning, so an older binary
/// on a newer schema shows everything else instead of failing the whole
/// list (ADR 0012). Every other error still fails the list.
pub(crate) fn readable<T>(
    rows: impl IntoIterator<Item = Result<T, DbError>>,
) -> Result<Vec<T>, DbError> {
    let mut out = Vec::new();
    for row in rows {
        match row {
            Ok(row) => out.push(row),
            Err(DbError::UnknownValue { what, value }) => {
                tracing::warn!(what, value, "row with an unknown value skipped");
            }
            Err(err) => return Err(err),
        }
    }
    Ok(out)
}

/// Inverse of [`ts`].
pub(crate) fn parse_ts(value: &str) -> Result<DateTime<Utc>, DbError> {
    DateTime::parse_from_rfc3339(value)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| DbError::InvalidData(format!("invalid timestamp `{value}`")))
}

/// Handle to the application database. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Database {
    pool: SqlitePool,
}

/// Migrations embedded at compile time. Use [`migrator`] to run them.
static EMBEDDED: Migrator = sqlx::migrate!("./migrations");

/// The migrator used at runtime: the embedded migrations, configured to ignore
/// applied migrations this binary does not know (written by a newer release).
fn migrator() -> Migrator {
    let mut migrator = Migrator::with_migrations(EMBEDDED.iter().cloned().collect());
    migrator.set_ignore_missing(true);
    migrator
}

impl Database {
    /// Open (or create) the database file at `path` and apply pending
    /// migrations. If the database already has applied migrations and new ones
    /// are pending, a backup is written to `backup_dir` first.
    pub async fn open(path: &Path, backup_dir: &Path) -> Result<Self, DbError> {
        let db = Self::open_with(path, backup_dir, &migrator()).await?;
        db.copy_column_keys().await?;
        Ok(db)
    }

    /// In-memory database for tests. Uses a single connection so all queries
    /// see the same database.
    pub async fn open_in_memory() -> Result<Self, DbError> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        migrator().run(&pool).await?;
        Ok(Self { pool })
    }

    async fn open_with(
        path: &Path,
        backup_dir: &Path,
        migrator: &Migrator,
    ) -> Result<Self, DbError> {
        // Held until the migrations ran; dropping the file releases it. Taken
        // before connecting, so creating the file and switching it to WAL
        // are serialised too (concurrent first opens otherwise hit
        // "database is locked").
        let _lock = migration_lock(path).await?;
        // Created 0600 before SQLite opens it; SQLite gives the WAL and
        // shared-memory files the database file's permissions.
        create_private(path).map_err(|source| DbError::CreateFile {
            path: path.to_path_buf(),
            source,
        })?;
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await?;

        let applied = applied_versions(&pool).await?;
        let first_pending = migrator
            .iter()
            .filter(|m| !m.migration_type.is_down_migration())
            .map(|m| m.version)
            .find(|version| !applied.contains(version));
        // A brand-new database (nothing applied yet) has nothing worth saving.
        if let Some(version) = first_pending
            && !applied.is_empty()
        {
            backup(&pool, backup_dir, version).await?;
        }

        migrator.run(&pool).await?;
        tracing::debug!("database migrations applied");
        Ok(Self { pool })
    }

    /// Cheap liveness check.
    pub async fn ping(&self) -> Result<(), DbError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Underlying pool, for repository modules inside this crate.
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Start a write transaction with `BEGIN IMMEDIATE`: it takes the write
    /// lock up front (waiting up to the busy timeout). A deferred transaction
    /// that reads first and writes later fails at once with
    /// `SQLITE_BUSY_SNAPSHOT` in WAL mode when another connection committed
    /// in between, and the busy timeout does not help there. Every
    /// transaction that writes uses this; pure reads stay deferred.
    pub(crate) async fn begin_write(
        &self,
    ) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>, DbError> {
        Ok(self.pool.begin_with("BEGIN IMMEDIATE").await?)
    }
}

/// Take the exclusive lock that serialises migrations between processes
/// sharing the database file, waiting while another process holds it.
async fn migration_lock(db_path: &Path) -> Result<File, DbError> {
    let mut path = db_path.as_os_str().to_owned();
    path.push(".migrate.lock");
    let path = PathBuf::from(path);
    let lock_err = |source| DbError::MigrationLock {
        path: path.clone(),
        source,
    };
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(lock_err)?;
    // Repeated every 30 s, so a hung migration elsewhere stays visible.
    let mut next_log = std::time::Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) => {
                if std::time::Instant::now() >= next_log {
                    tracing::info!(path = %path.display(), "another Starr process is migrating the database; waiting");
                    next_log += Duration::from_secs(30);
                }
                // Async wait: the lock holder may be a task on this runtime.
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(TryLockError::Error(err)) => return Err(lock_err(err)),
        }
    }
}

/// Versions recorded in sqlx's migration table; empty if it does not exist yet.
async fn applied_versions(pool: &SqlitePool) -> Result<Vec<i64>, DbError> {
    let table: Option<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?;
    if table.is_none() {
        return Ok(Vec::new());
    }
    let versions: Vec<(i64,)> = sqlx::query_as("SELECT version FROM _sqlx_migrations")
        .fetch_all(pool)
        .await?;
    Ok(versions.into_iter().map(|(v,)| v).collect())
}

/// Write `starr-<UTC timestamp>-pre-<version>.db` into `dir` and prune old backups.
async fn backup(pool: &SqlitePool, dir: &Path, first_pending: i64) -> Result<(), DbError> {
    let io_err = |source| DbError::Backup {
        path: dir.to_path_buf(),
        source,
    };
    std::fs::create_dir_all(dir).map_err(io_err)?;

    // A failing migration restarted in a loop (Docker restart policy) must not
    // push older backups out of the retention window: one backup per version.
    let suffix = format!("-pre-{first_pending}.db");
    // An empty file is what a failed `VACUUM INTO` left behind: no backup.
    if list_backups(dir).map_err(io_err)?.iter().any(|p| {
        p.to_str().is_some_and(|p| p.ends_with(&suffix))
            && std::fs::metadata(p).is_ok_and(|m| m.len() > 0)
    }) {
        tracing::info!(
            version = first_pending,
            "backup for this migration already exists"
        );
        return Ok(());
    }

    let (timestamp,): (String,) = sqlx::query_as("SELECT strftime('%Y%m%dT%H%M%SZ', 'now')")
        .fetch_one(pool)
        .await?;
    let target = dir.join(format!("starr-{timestamp}-pre-{first_pending}.db"));
    let Some(target_str) = target.to_str() else {
        return Err(DbError::NonUtf8Path(target));
    };
    // `VACUUM INTO` accepts an empty file: created 0600 first, so the copy
    // is private from the start.
    create_private(&target).map_err(io_err)?;
    if let Err(err) = sqlx::query("VACUUM INTO ?")
        .bind(target_str)
        .execute(pool)
        .await
    {
        let _ = std::fs::remove_file(&target);
        return Err(err.into());
    }
    tracing::info!(path = %target.display(), "database backed up before migration");

    prune_backups(dir, BACKUPS_TO_KEEP).map_err(io_err)
}

/// Create `path` as an empty file only its owner can read and write (0600),
/// unless it exists already; an existing file keeps its permissions.
fn create_private(path: &Path) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err),
    }
}

/// Delete all but the newest `keep` backups. Names start with a sortable UTC
/// timestamp, so lexical order is chronological order.
fn prune_backups(dir: &Path, keep: usize) -> std::io::Result<()> {
    let backups = list_backups(dir)?;
    let excess = backups.len().saturating_sub(keep);
    for old in &backups[..excess] {
        std::fs::remove_file(old)?;
        tracing::debug!(path = %old.display(), "removed old database backup");
    }
    Ok(())
}

/// Backup files in `dir`, oldest first.
fn list_backups(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut backups = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if is_backup_file(&path) {
            backups.push(path);
        }
    }
    backups.sort();
    Ok(backups)
}

fn is_backup_file(path: &Path) -> bool {
    path.extension() == Some(OsStr::new("db"))
        && path
            .file_stem()
            .and_then(OsStr::to_str)
            .is_some_and(|stem| stem.starts_with("starr-") && stem.contains("-pre-"))
}

#[cfg(test)]
mod tests {
    use sqlx::SqlSafeStr;
    use sqlx::migrate::{Migration, MigrationType};

    use super::*;
    use crate::test_support::TempDir;

    const NEXT_VERSION: i64 = 99_990_101_000_000;

    #[test]
    fn readable_skips_unknown_values_and_keeps_other_errors() {
        let rows = vec![Ok(1), Err(unknown("state", "teleported")), Ok(3)];
        assert_eq!(readable(rows).unwrap(), vec![1, 3]);
        let broken = vec![Ok(1), Err(DbError::InvalidData("bad".into()))];
        assert!(matches!(readable(broken), Err(DbError::InvalidData(_))));
    }

    /// The embedded migrations plus one more, simulating a newer release.
    fn newer_migrator() -> Migrator {
        let mut migrations: Vec<Migration> = EMBEDDED.iter().cloned().collect();
        migrations.push(Migration::new(
            NEXT_VERSION,
            "next".into(),
            MigrationType::Simple,
            "CREATE TABLE next_thing (id INTEGER PRIMARY KEY) STRICT".into_sql_str(),
            false,
        ));
        let mut migrator = Migrator::with_migrations(migrations);
        migrator.set_ignore_missing(true);
        migrator
    }

    async fn open_and_close(dir: &TempDir, migrator: &Migrator) {
        let db = Database::open_with(&dir.db(), &dir.backups(), migrator)
            .await
            .unwrap();
        db.pool().close().await;
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// L4: the database, its WAL and shared-memory files and the
    /// pre-migration backups hold password hashes and personal data; only
    /// Starr's user may read them, whatever the umask.
    #[cfg(unix)]
    #[tokio::test]
    async fn database_wal_and_backups_are_private_to_the_owner() {
        let dir = TempDir::new();
        let db = Database::open_with(&dir.db(), &dir.backups(), &migrator())
            .await
            .unwrap();
        sqlx::query("CREATE TABLE touch (id INTEGER) STRICT")
            .execute(db.pool())
            .await
            .unwrap();
        let db_path = dir.db();
        let mut wal = db_path.as_os_str().to_owned();
        wal.push("-wal");
        let mut shm = db_path.as_os_str().to_owned();
        shm.push("-shm");
        for path in [db_path.clone(), wal.into(), shm.into()] {
            assert_eq!(mode(&path), 0o600, "{}", path.display());
        }
        db.pool().close().await;

        open_and_close(&dir, &newer_migrator()).await;
        let backups = list_backups(&dir.backups()).unwrap();
        assert_eq!(backups.len(), 1);
        assert_eq!(mode(&backups[0]), 0o600);
    }

    /// An empty backup file (left by a `VACUUM INTO` that failed) does not
    /// count as the backup for that migration.
    #[tokio::test]
    async fn an_empty_leftover_backup_is_not_taken_for_a_backup() {
        let dir = TempDir::new();
        open_and_close(&dir, &migrator()).await;
        std::fs::create_dir_all(dir.backups()).unwrap();
        let leftover = dir
            .backups()
            .join(format!("starr-20000101T000000Z-pre-{NEXT_VERSION}.db"));
        std::fs::write(&leftover, b"").unwrap();
        open_and_close(&dir, &newer_migrator()).await;
        let real = list_backups(&dir.backups())
            .unwrap()
            .into_iter()
            .filter(|p| std::fs::metadata(p).unwrap().len() > 0)
            .count();
        assert_eq!(real, 1);
    }

    /// Adding `import_blocked` rebuilds the notification table: existing
    /// notifications and their deliveries survive it.
    #[tokio::test]
    async fn notification_rebuild_keeps_notifications_and_deliveries() {
        const REBUILD: i64 = 20_261_112_100_000;
        let dir = TempDir::new();
        let before = Migrator::with_migrations(
            EMBEDDED
                .iter()
                .filter(|m| m.version < REBUILD)
                .cloned()
                .collect(),
        );
        let db = Database::open_with(&dir.db(), &dir.backups(), &before)
            .await
            .unwrap();
        sqlx::raw_sql(
            "INSERT INTO notification_channel (id, kind, name, secret_ciphertext,
                 secret_recipient, admin_events, created_at, updated_at)
             VALUES (1, 'webhook', 'hook', x'00', 'age1x', '[\"acquisition_failed\"]', 't', 't');
             INSERT INTO notification (id, member_id, kind, payload, dedup_key, created_at)
             VALUES (7, NULL, 'acquisition_failed', '{}', 'k', 't');
             INSERT INTO notification_delivery (id, notification_id, channel_id, state,
                 created_at, updated_at)
             VALUES (3, 7, 1, 'pending', 't', 't');",
        )
        .execute(db.pool())
        .await
        .unwrap();
        db.pool().close().await;

        let db = Database::open(&dir.db(), &dir.backups()).await.unwrap();
        let delivery: (i64, i64, String) =
            sqlx::query_as("SELECT id, notification_id, state FROM notification_delivery")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(delivery, (3, 7, "pending".to_owned()));
        // The reference moved to the rebuilt table: deleting the
        // notification still takes its deliveries along.
        sqlx::query("DELETE FROM notification WHERE id = 7")
            .execute(db.pool())
            .await
            .unwrap();
        let (left,): (i64,) = sqlx::query_as("SELECT count(*) FROM notification_delivery")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(left, 0);
        sqlx::query(
            "INSERT INTO notification (member_id, kind, payload, dedup_key, created_at)
             VALUES (NULL, 'import_blocked', '{}', 'b', 't')",
        )
        .execute(db.pool())
        .await
        .unwrap();
    }

    /// Removing requests (spec 0004) rebuilds the request tables and the
    /// notification table: every row survives, the references point at the
    /// rebuilt tables, and existing acquisitions count as "not changed by
    /// Starr" (never unmonitored).
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one migration story
    async fn request_rebuild_keeps_requests_items_events_and_acquisitions() {
        const REBUILD: i64 = 20_261_113_100_000;
        let dir = TempDir::new();
        let before = Migrator::with_migrations(
            EMBEDDED
                .iter()
                .filter(|m| m.version < REBUILD)
                .cloned()
                .collect(),
        );
        let db = Database::open_with(&dir.db(), &dir.backups(), &before)
            .await
            .unwrap();
        let t = chrono::DateTime::UNIX_EPOCH;
        let mia = db
            .create_member(
                &crate::members::NewMember {
                    username: "mia",
                    email: "mia@example.com",
                    display_name: "Mia",
                    password_hash: None,
                },
                &[] as &[starr_core::members::Entitlement],
                t,
            )
            .await
            .unwrap();
        assert_eq!(mia, 1);
        let radarr = db
            .create_service(
                &crate::services::NewServiceInstance {
                    adapter_type: "radarr",
                    name: "Radarr",
                    base_url: "http://r",
                    public_url: None,
                    credential: &crate::services::StoredCredential {
                        ciphertext: b"c".to_vec(),
                        recipient: "age1x".to_owned(),
                        class: starr_core::adapters::SecretClass::ServiceCredential,
                    },
                },
                t,
            )
            .await
            .unwrap()
            .id;
        assert_eq!(radarr, 1);
        sqlx::raw_sql(
            "INSERT INTO catalog_item (id, kind, tmdb_id, title, created_at, updated_at)
             VALUES (3, 'movie', 603, 'The Matrix', 't', 't');
             INSERT INTO acquisition (id, catalog_item_id, service_instance_id, category, state,
                 quality_profile_id, root_folder_path, created_at, updated_at, download_stage)
             VALUES (4, 3, 1, 'movies', 'downloading', 1, '/movies', 't', 't', 'importing');
             INSERT INTO request (id, member_id, catalog_item_id, category, state, created_at,
                 updated_at)
             VALUES (5, 1, 3, 'movies', 'in_progress', 't', 't');
             INSERT INTO request_item (id, request_id, member_id, catalog_item_id, decision,
                 acquisition_id, decided_at)
             VALUES (6, 5, 1, 3, 'approved', 4, 't');
             INSERT INTO request_event (id, request_id, at, to_state, trigger, actor)
             VALUES (7, 5, 't', 'in_progress', 'system', 'acquisitions.sync');
             INSERT INTO notification (id, member_id, kind, payload, dedup_key, created_at)
             VALUES (8, 1, 'request_approved', '{}', 'k', 't');",
        )
        .execute(db.pool())
        .await
        .unwrap();
        db.pool().close().await;

        let db = Database::open(&dir.db(), &dir.backups()).await.unwrap();
        let acquisition: (String, Option<String>, bool, bool, Option<String>) = sqlx::query_as(
            "SELECT state, download_stage, starr_added, starr_monitored, stop_requested_at
             FROM acquisition WHERE id = 4",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(
            acquisition,
            (
                "downloading".into(),
                Some("importing".into()),
                false,
                false,
                None
            )
        );
        let item: (i64, String, Option<i64>) =
            sqlx::query_as("SELECT request_id, decision, acquisition_id FROM request_item")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(item, (5, "approved".into(), Some(4)));
        let (events,): (i64,) = sqlx::query_as("SELECT count(*) FROM request_event")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(events, 1);
        // The new endings are accepted.
        sqlx::query("UPDATE request_item SET decision = 'revoked', ended_at = 't' WHERE id = 6")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE request SET state = 'revoked', hidden_at = 't' WHERE id = 5")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE acquisition SET state = 'cancelled' WHERE id = 4")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO notification (member_id, kind, payload, dedup_key, created_at)
             VALUES (1, 'request_revoked', '{}', 'r', 't')",
        )
        .execute(db.pool())
        .await
        .unwrap();
        // The references moved to the rebuilt tables: deleting the member
        // takes the request, its items and events and notifications along.
        sqlx::query("DELETE FROM member WHERE id = 1")
            .execute(db.pool())
            .await
            .unwrap();
        for table in ["request", "request_item", "request_event", "notification"] {
            let (left,): (i64,) =
                sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(left, 0, "{table}");
        }
        let (fk,): (i64,) = sqlx::query_as("SELECT count(*) FROM pragma_foreign_key_check")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(fk, 0);
    }

    /// Catalog identity (ADR 0012) rebuilds `catalog_item` and every table
    /// referencing it: all rows survive with their ids, the natural keys
    /// are copied into `catalog_key` (a key two items claimed stays with the
    /// older one), the references point at the rebuilt tables, and the
    /// new kinds and the follow date cutoff are accepted.
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one migration story
    async fn catalog_identity_keeps_items_references_and_copies_keys() {
        const REBUILD: i64 = 20_261_114_100_000;
        let dir = TempDir::new();
        let before = Migrator::with_migrations(
            EMBEDDED
                .iter()
                .filter(|m| m.version < REBUILD)
                .cloned()
                .collect(),
        );
        let db = Database::open_with(&dir.db(), &dir.backups(), &before)
            .await
            .unwrap();
        sqlx::raw_sql(
            "INSERT INTO member (id, username, email, display_name, created_at)
             VALUES (1, 'mia', 'mia@example.com', 'Mia', 't');
             INSERT INTO service_instance (id, adapter_type, name, base_url, credential_sealed,
                 credential_recipient, credential_class, created_at, updated_at)
             VALUES (1, 'sonarr', 'Sonarr', 'http://s', x'00', 'age1x', 'service', 't', 't');
             INSERT INTO catalog_item (id, kind, tmdb_id, imdb_id, title, created_at, updated_at)
             VALUES (2, 'movie', 603, 'tt0133093', 'The Matrix', 't', 't');
             INSERT INTO catalog_item (id, kind, tmdb_id, tvdb_id, imdb_id, title, created_at,
                 updated_at)
             VALUES (3, 'series', 1399, 121361, 'tt0944947', 'Game of Thrones', 't', 't');
             INSERT INTO catalog_item (id, kind, parent_id, season_number, title, metadata,
                 metadata_source, metadata_fetched_at, metadata_expires_at, created_at, updated_at)
             VALUES (4, 'season', 3, 1, 'Season 1', '{\"episodeCount\": 10}', 'tmdb',
                 '2026-11-01T00:00:00.000Z', '2026-12-01T00:00:00.000Z', 't', 't');
             -- A movie edit sharing the series' IMDb id (allowed per kind before).
             INSERT INTO catalog_item (id, kind, tmdb_id, imdb_id, title, created_at, updated_at)
             VALUES (5, 'movie', 999, 'tt0944947', 'Game of Thrones (cut)', 't', 't');
             INSERT INTO external_ref (catalog_item_id, service_instance_id, remote_id,
                 confirmed_at)
             VALUES (3, 1, '17', 't');
             INSERT INTO acquisition (id, catalog_item_id, service_instance_id, category, state,
                 quality_profile_id, root_folder_path, created_at, updated_at, starr_added,
                 starr_monitored)
             VALUES (6, 4, 1, 'series', 'submitted', 1, '/tv', 't', 't', 1, 1);
             INSERT INTO request (id, member_id, catalog_item_id, category, state, follow,
                 created_at, updated_at)
             VALUES (7, 1, 3, 'series', 'submitted', 1, 't', 't');
             INSERT INTO request_item (id, request_id, member_id, catalog_item_id, season_number,
                 decision, acquisition_id, decided_at)
             VALUES (8, 7, 1, 4, 1, 'approved', 6, 't');
             INSERT INTO request_event (id, request_id, at, to_state, trigger, actor)
             VALUES (9, 7, 't', 'submitted', 'system', 'requests.submit');
             INSERT INTO follow (id, member_id, catalog_item_id, category, seasons_through,
                 created_at)
             VALUES (10, 1, 3, 'series', 1, 't');",
        )
        .execute(db.pool())
        .await
        .unwrap();
        db.pool().close().await;

        let db = Database::open(&dir.db(), &dir.backups()).await.unwrap();
        let items: Vec<(i64, String, Option<i64>, Option<i64>)> = sqlx::query_as(
            "SELECT id, kind, parent_id, season_number FROM catalog_item ORDER BY id",
        )
        .fetch_all(db.pool())
        .await
        .unwrap();
        assert_eq!(
            items,
            vec![
                (2, "movie".into(), None, None),
                (3, "series".into(), None, None),
                (4, "season".into(), Some(3), Some(1)),
                (5, "movie".into(), None, None),
            ]
        );
        let keys: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT catalog_item_id, scheme, value FROM catalog_key
             ORDER BY catalog_item_id, scheme",
        )
        .fetch_all(db.pool())
        .await
        .unwrap();
        let key = |id: i64, scheme: &str, value: &str| (id, scheme.to_owned(), value.to_owned());
        assert_eq!(
            keys,
            vec![
                key(2, "imdb", "tt0133093"),
                key(2, "tmdb_movie", "603"),
                key(3, "imdb", "tt0944947"),
                key(3, "tmdb_tv", "1399"),
                key(3, "tvdb", "121361"),
                key(5, "tmdb_movie", "999"),
            ]
        );
        // Every other row is still there, linked as before.
        let (remote,): (String,) =
            sqlx::query_as("SELECT remote_id FROM external_ref WHERE catalog_item_id = 3")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(remote, "17");
        let linked: (i64, i64, String, i64, bool) = sqlx::query_as(
            "SELECT i.request_id, i.acquisition_id, a.state, a.catalog_item_id, a.starr_added
             FROM request_item i JOIN acquisition a ON a.id = i.acquisition_id WHERE i.id = 8",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(linked, (7, 6, "submitted".into(), 4, true));
        let follow: (i64, Option<i64>, Option<String>) =
            sqlx::query_as("SELECT catalog_item_id, seasons_through, known_through FROM follow")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(follow, (3, Some(1), None));
        let (events,): (i64,) = sqlx::query_as("SELECT count(*) FROM request_event")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(events, 1);

        // New kinds follow the generic parent rule.
        sqlx::raw_sql(
            "INSERT INTO catalog_item (id, kind, title, created_at, updated_at)
             VALUES (20, 'artist', 'Radiohead', 't', 't');
             INSERT INTO catalog_item (id, kind, parent_id, title, created_at, updated_at)
             VALUES (21, 'album', 20, 'OK Computer', 't', 't');
             INSERT INTO catalog_key (catalog_item_id, scheme, value)
             VALUES (20, 'mb_artist', 'a74b1b7f-71a5-4011-9441-d0b5e4122711');
             INSERT INTO follow (member_id, catalog_item_id, category, known_through, created_at)
             VALUES (1, 20, 'music', '2026-11-02', 't');",
        )
        .execute(db.pool())
        .await
        .unwrap();
        for bad in [
            "INSERT INTO catalog_item (kind, title, created_at, updated_at)
             VALUES ('album', 'No artist', 't', 't')",
            "INSERT INTO catalog_item (kind, parent_id, season_number, title, created_at, updated_at)
             VALUES ('album', 20, 1, 'Numbered', 't', 't')",
            "INSERT INTO catalog_item (kind, parent_id, title, created_at, updated_at)
             VALUES ('scene', 20, 'Child scene', 't', 't')",
            "INSERT INTO catalog_item (kind, title, created_at, updated_at)
             VALUES ('track', 'Airbag', 't', 't')",
            "INSERT INTO catalog_key (catalog_item_id, scheme, value)
             VALUES (21, 'mb_release_group', 'NOT-A-UUID')",
            "INSERT INTO catalog_key (catalog_item_id, scheme, value)
             VALUES (21, 'tmdb_movie', '0603')",
            "INSERT INTO catalog_key (catalog_item_id, scheme, value)
             VALUES (21, 'imdb', 'tt12x')",
            // A key names one item only.
            "INSERT INTO catalog_key (catalog_item_id, scheme, value)
             VALUES (21, 'mb_artist', 'a74b1b7f-71a5-4011-9441-d0b5e4122711')",
            "INSERT INTO follow (member_id, catalog_item_id, category, created_at)
             VALUES (1, 21, 'music', 't')",
        ] {
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(bad.to_owned()))
                    .execute(db.pool())
                    .await
                    .is_err(),
                "{bad}"
            );
        }
        // A scheme this release does not know yet needs no rebuild.
        sqlx::query(
            "INSERT INTO catalog_key (catalog_item_id, scheme, value) VALUES (21, 'isrc', 'X1')",
        )
        .execute(db.pool())
        .await
        .unwrap();

        // The references moved to the rebuilt tables: deleting the member
        // takes requests, items, events and follows along; deleting an
        // item takes its keys and external references.
        sqlx::query("DELETE FROM member WHERE id = 1")
            .execute(db.pool())
            .await
            .unwrap();
        for table in ["request", "request_item", "request_event", "follow"] {
            let (left,): (i64,) =
                sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(left, 0, "{table}");
        }
        sqlx::query("DELETE FROM acquisition")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("DELETE FROM catalog_item WHERE id = 3")
            .execute(db.pool())
            .await
            .unwrap();
        let (left,): (i64,) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM catalog_key WHERE catalog_item_id IN (3, 4))
                  + (SELECT count(*) FROM external_ref)
                  + (SELECT count(*) FROM catalog_item WHERE id = 4)",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(left, 0);
        let (fk,): (i64,) = sqlx::query_as("SELECT count(*) FROM pragma_foreign_key_check")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(fk, 0);
    }

    #[tokio::test]
    async fn migrations_apply_and_ping_works() {
        let db = Database::open_in_memory().await.unwrap();
        db.ping().await.unwrap();
        let created: (String,) =
            sqlx::query_as("SELECT value FROM app_meta WHERE key = 'created_at'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!(
            created.0.starts_with("20"),
            "unexpected created_at: {}",
            created.0
        );
    }

    /// Reading first and writing later in one transaction must not fail
    /// with `SQLITE_BUSY_SNAPSHOT` (WAL) when another connection writes in
    /// between: the write transaction holds the write lock from the start,
    /// so the other writer cannot commit until it is done.
    #[tokio::test]
    async fn write_transaction_survives_concurrent_writer() {
        let dir = TempDir::new();
        let db = Database::open(&dir.db(), &dir.backups()).await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let _: Option<(String,)> =
            sqlx::query_as("SELECT value FROM app_meta WHERE key = 'created_at'")
                .fetch_optional(&mut *tx)
                .await
                .unwrap();

        // Another writer that does not wait: it must find the write lock
        // taken. With a deferred transaction it would commit here, and the
        // write below would then fail with SQLITE_BUSY_SNAPSHOT.
        let mut other = db.pool().acquire().await.unwrap();
        sqlx::query("PRAGMA busy_timeout = 0")
            .execute(&mut *other)
            .await
            .unwrap();
        let err = sqlx::query("INSERT INTO app_meta (key, value) VALUES ('race_other', 'x')")
            .execute(&mut *other)
            .await
            .unwrap_err();
        let code = err
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code);
        assert_eq!(code.as_deref(), Some("5"), "expected SQLITE_BUSY: {err}");
        // Back to the pool's setting before the connection is returned.
        sqlx::query("PRAGMA busy_timeout = 5000")
            .execute(&mut *other)
            .await
            .unwrap();
        drop(other);

        sqlx::query("INSERT INTO app_meta (key, value) VALUES ('race_tx', 'y')")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn fresh_database_is_not_backed_up() {
        let dir = TempDir::new();
        open_and_close(&dir, &migrator()).await;
        assert!(dir.backup_names().is_empty(), "{:?}", dir.backup_names());
    }

    #[tokio::test]
    async fn reopening_without_pending_migrations_does_not_back_up() {
        let dir = TempDir::new();
        open_and_close(&dir, &migrator()).await;
        open_and_close(&dir, &migrator()).await;
        assert!(dir.backup_names().is_empty(), "{:?}", dir.backup_names());
    }

    #[tokio::test]
    async fn pending_migrations_on_existing_database_are_backed_up_first() {
        let dir = TempDir::new();
        open_and_close(&dir, &migrator()).await;
        open_and_close(&dir, &newer_migrator()).await;

        let names = dir.backup_names();
        assert_eq!(names.len(), 1, "{names:?}");
        assert!(
            names[0].starts_with("starr-")
                && names[0].ends_with(&format!("-pre-{NEXT_VERSION}.db")),
            "{names:?}"
        );

        // The backup holds the pre-migration schema.
        let backup = Database::open_with(
            &dir.backups().join(&names[0]),
            &dir.0.join("unused"),
            &migrator(),
        )
        .await
        .unwrap();
        let next: Option<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'next_thing'",
        )
        .fetch_optional(backup.pool())
        .await
        .unwrap();
        assert!(next.is_none(), "backup must predate the new migration");
    }

    #[tokio::test]
    async fn repeated_attempts_at_the_same_migration_back_up_once() {
        let dir = TempDir::new();
        open_and_close(&dir, &migrator()).await;
        std::fs::create_dir_all(dir.backups()).unwrap();
        // An earlier start already backed up before this migration, then failed.
        let earlier = format!("starr-20200101T000000Z-pre-{NEXT_VERSION}.db");
        std::fs::write(dir.backups().join(&earlier), b"earlier").unwrap();

        open_and_close(&dir, &newer_migrator()).await;

        assert_eq!(dir.backup_names(), vec![earlier]);
    }

    #[tokio::test]
    async fn only_the_newest_five_backups_are_kept() {
        let dir = TempDir::new();
        open_and_close(&dir, &migrator()).await;
        std::fs::create_dir_all(dir.backups()).unwrap();
        for i in 0..6 {
            let name = format!("starr-20200101T00000{i}Z-pre-1.db");
            std::fs::write(dir.backups().join(name), b"old").unwrap();
        }
        std::fs::write(dir.backups().join("unrelated.txt"), b"keep me").unwrap();

        open_and_close(&dir, &newer_migrator()).await;

        let names = dir.backup_names();
        let backups: Vec<&String> = names.iter().filter(|n| n.starts_with("starr-")).collect();
        assert_eq!(backups.len(), 5, "{names:?}");
        assert!(!names.contains(&"starr-20200101T000000Z-pre-1.db".to_owned()));
        assert!(!names.contains(&"starr-20200101T000001Z-pre-1.db".to_owned()));
        assert!(
            backups
                .iter()
                .any(|n| n.ends_with(&format!("-pre-{NEXT_VERSION}.db")))
        );
        assert!(names.contains(&"unrelated.txt".to_owned()));
    }

    /// Web and worker containers starting at the same time (split mode).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_opens_of_a_new_database_both_succeed() {
        let dir = TempDir::new();
        let (path, backups) = (dir.db(), dir.backups());
        let (a, b) = tokio::join!(
            Database::open(&path, &backups),
            Database::open(&path, &backups),
        );
        let (a, b) = (a.unwrap(), b.unwrap());
        a.ping().await.unwrap();
        b.ping().await.unwrap();
        let (applied,): (i64,) = sqlx::query_as("SELECT count(*) FROM _sqlx_migrations")
            .fetch_one(b.pool())
            .await
            .unwrap();
        assert_eq!(applied, i64::try_from(EMBEDDED.iter().count()).unwrap());
        assert!(dir.backup_names().is_empty(), "{:?}", dir.backup_names());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_upgrades_migrate_and_back_up_once() {
        let dir = TempDir::new();
        open_and_close(&dir, &migrator()).await;
        let newer = newer_migrator();
        let (path, backups) = (dir.db(), dir.backups());
        let (a, b) = tokio::join!(
            Database::open_with(&path, &backups, &newer),
            Database::open_with(&path, &backups, &newer),
        );
        a.unwrap();
        b.unwrap();
        let names = dir.backup_names();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[tokio::test]
    async fn unknown_applied_migration_from_newer_release_is_tolerated() {
        let dir = TempDir::new();
        let db = Database::open(&dir.db(), &dir.backups()).await.unwrap();
        sqlx::query(
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
             VALUES (?, 'from the future', TRUE, X'00', 0)",
        )
        .bind(NEXT_VERSION)
        .execute(db.pool())
        .await
        .unwrap();
        db.pool().close().await;

        let db = Database::open(&dir.db(), &dir.backups()).await.unwrap();
        db.ping().await.unwrap();
        assert!(dir.backup_names().is_empty(), "{:?}", dir.backup_names());
    }
}
