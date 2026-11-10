//! Catalog response cache (spec 0003 slice 1): response bodies of metadata
//! services per instance and request, with an expiry the adapter kit
//! derives from `Cache-Control` (at most six months, TMDB terms).

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::jobs::delta;
use crate::{Database, DbError, ts};

impl Database {
    /// The cached body of `request` for instance `instance_id`, if it has
    /// not expired at `now`.
    pub async fn catalog_cache_get(
        &self,
        instance_id: i64,
        request: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Vec<u8>>, DbError> {
        let row: Option<(Vec<u8>,)> = sqlx::query_as(
            "SELECT body FROM catalog_cache
             WHERE service_instance_id = ?1 AND request = ?2 AND expires_at > ?3",
        )
        .bind(instance_id)
        .bind(request)
        .bind(ts(now))
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|(body,)| body))
    }

    /// Store (or replace) the body of `request`, valid for `ttl` from `now`.
    /// Nothing is stored for an instance that no longer exists.
    pub async fn catalog_cache_put(
        &self,
        instance_id: i64,
        request: &str,
        body: &[u8],
        now: DateTime<Utc>,
        ttl: Duration,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO catalog_cache (service_instance_id, request, body, fetched_at, expires_at)
             SELECT ?1, ?2, ?3, ?4, ?5 WHERE EXISTS (SELECT 1 FROM service_instance WHERE id = ?1)
             ON CONFLICT (service_instance_id, request) DO UPDATE SET
                 body = excluded.body,
                 fetched_at = excluded.fetched_at,
                 expires_at = excluded.expires_at",
        )
        .bind(instance_id)
        .bind(request)
        .bind(body)
        .bind(ts(now))
        .bind(ts(now + delta(ttl)))
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Housekeeping: delete rows expired at `now`, then the soonest-expiring
    /// rows beyond `max_rows`. Returns the number of rows deleted.
    pub async fn catalog_cache_prune(
        &self,
        now: DateTime<Utc>,
        max_rows: u32,
    ) -> Result<u64, DbError> {
        let expired = sqlx::query("DELETE FROM catalog_cache WHERE expires_at <= ?1")
            .bind(ts(now))
            .execute(self.pool())
            .await?
            .rows_affected();
        let over = sqlx::query(
            "DELETE FROM catalog_cache WHERE rowid IN (
                 SELECT rowid FROM catalog_cache ORDER BY expires_at DESC LIMIT -1 OFFSET ?1
             )",
        )
        .bind(i64::from(max_rows))
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(expired + over)
    }

    /// Delete every row whose request key starts with `prefix` (the
    /// private category's catalog rows, spec 0007 Q13: purged when the
    /// category is switched off). Returns the number of rows deleted.
    pub async fn catalog_cache_purge_prefix(&self, prefix: &str) -> Result<u64, DbError> {
        Ok(
            sqlx::query("DELETE FROM catalog_cache WHERE substr(request, 1, length(?1)) = ?1")
                .bind(prefix)
                .execute(self.pool())
                .await?
                .rows_affected(),
        )
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;
    use crate::services::{NewServiceInstance, StoredCredential};
    use crate::test_support::t0;

    async fn tmdb(db: &Database) -> i64 {
        db.create_service(
            &NewServiceInstance {
                adapter_type: "tmdb",
                name: "TMDB",
                base_url: "https://api.themoviedb.org",
                public_url: None,
                credential: &StoredCredential {
                    class: starr_core::adapters::SecretClass::CatalogReadToken,
                    ciphertext: b"sealed".to_vec(),
                    recipient: "age1test".to_owned(),
                },
            },
            t0(),
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn bodies_expire_and_are_replaced() {
        let db = Database::open_in_memory().await.unwrap();
        let id = tmdb(&db).await;
        let ttl = Duration::from_mins(10);
        db.catalog_cache_put(id, "3/movie/1?language=en-US", b"v1", t0(), ttl)
            .await
            .unwrap();
        let get = |at| db.catalog_cache_get(id, "3/movie/1?language=en-US", at);
        assert_eq!(get(t0()).await.unwrap(), Some(b"v1".to_vec()));
        assert_eq!(
            get(t0() + TimeDelta::minutes(10)).await.unwrap(),
            None,
            "expired at exactly the TTL"
        );
        db.catalog_cache_put(id, "3/movie/1?language=en-US", b"v2", t0(), ttl)
            .await
            .unwrap();
        assert_eq!(get(t0()).await.unwrap(), Some(b"v2".to_vec()));
        // Keyed by instance.
        assert_eq!(
            db.catalog_cache_get(id + 1, "3/movie/1?language=en-US", t0())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn rows_go_with_their_instance_and_unknown_instances_store_nothing() {
        let db = Database::open_in_memory().await.unwrap();
        let id = tmdb(&db).await;
        let ttl = Duration::from_mins(10);
        db.catalog_cache_put(id, "a", b"x", t0(), ttl)
            .await
            .unwrap();
        db.catalog_cache_put(id + 99, "a", b"x", t0(), ttl)
            .await
            .unwrap();
        assert!(db.delete_service(id, t0()).await.unwrap());
        let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM catalog_cache")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn a_prefix_purges_its_rows_only() {
        let db = Database::open_in_memory().await.unwrap();
        let id = tmdb(&db).await;
        let ttl = Duration::from_mins(10);
        for key in ["adult:1", "adult:2", "3/movie/1", "adult_x", "xadult:"] {
            db.catalog_cache_put(id, key, b"x", t0(), ttl)
                .await
                .unwrap();
        }
        assert_eq!(db.catalog_cache_purge_prefix("adult:").await.unwrap(), 2);
        assert_eq!(
            db.catalog_cache_get(id, "adult:1", t0()).await.unwrap(),
            None
        );
        for key in ["3/movie/1", "adult_x", "xadult:"] {
            assert!(
                db.catalog_cache_get(id, key, t0()).await.unwrap().is_some(),
                "{key}"
            );
        }
    }

    #[tokio::test]
    async fn prune_deletes_expired_then_caps_by_expiry() {
        let db = Database::open_in_memory().await.unwrap();
        let id = tmdb(&db).await;
        for (key, mins) in [("old", 1), ("a", 30), ("b", 60), ("c", 90)] {
            db.catalog_cache_put(id, key, b"x", t0(), Duration::from_mins(mins))
                .await
                .unwrap();
        }
        let now = t0() + TimeDelta::minutes(5);
        assert_eq!(db.catalog_cache_prune(now, 2).await.unwrap(), 2);
        assert_eq!(db.catalog_cache_get(id, "a", now).await.unwrap(), None);
        assert!(db.catalog_cache_get(id, "b", now).await.unwrap().is_some());
        assert!(db.catalog_cache_get(id, "c", now).await.unwrap().is_some());
        assert_eq!(db.catalog_cache_prune(now, 2).await.unwrap(), 0);
    }
}
