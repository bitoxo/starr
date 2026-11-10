//! Helpers shared by this crate's tests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use chrono::{DateTime, TimeZone, Utc};

/// A fixed point in time for deterministic queue tests.
pub fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0).unwrap()
}

/// A fresh, empty directory under the system temp dir, removed on drop.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("starr-db-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    pub fn db(&self) -> PathBuf {
        self.0.join("starr.db")
    }
    pub fn backups(&self) -> PathBuf {
        self.0.join("backups")
    }
    pub fn backup_names(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.backups()) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run `sql` as a newer release would: with values this binary's CHECK
/// constraints do not allow (ADR 0008 rollback, ADR 0012).
pub async fn write_as_newer_release(db: &crate::Database, sql: &str) {
    let mut conn = db.pool().acquire().await.unwrap();
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_owned()))
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("PRAGMA ignore_check_constraints = OFF")
        .execute(&mut *conn)
        .await
        .unwrap();
}

/// Copy the key columns of raw-inserted catalog items into `catalog_key`
/// (what the repository writes itself; ADR 0012).
pub async fn keys_from_columns(db: &crate::Database) {
    db.copy_column_keys().await.unwrap();
}
