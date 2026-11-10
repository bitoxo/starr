//! Libraries of consume instances (spec 0005 M4, slice 2): the last library
//! listing per instance, written by `accounts.scan`, and the admin's
//! "belongs to no category" flag. Which libraries belong to which category
//! is stored in the consume role's settings
//! ([`crate::categories::ServiceRole::consume_settings`]).

use chrono::{DateTime, Utc};
use starr_core::adapters::Listing;
use starr_core::library::{LibraryContent, MediaLibrary};

use crate::{Database, DbError, ts};

/// One library of an instance as last listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceLibrary {
    pub library: MediaLibrary,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    /// Since when an admin marked it as belonging to no category.
    pub ignored_at: Option<DateTime<Utc>>,
    /// Username of that admin.
    pub ignored_by: Option<String>,
}

/// The last library listing of an instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryListingState {
    /// Last attempt; `None` if none ran yet.
    pub listed_at: Option<DateTime<Utc>>,
    /// Whether the last successful listing was provably complete; `None`
    /// when the last attempt failed or none ran.
    pub complete: Option<bool>,
    /// Error code of the last attempt; `None` when it succeeded.
    pub error: Option<String>,
}

/// Stable name of a library content in the database.
#[must_use]
pub const fn content_name(content: LibraryContent) -> &'static str {
    match content {
        LibraryContent::Movies => "movies",
        LibraryContent::Series => "series",
        LibraryContent::Music => "music",
        LibraryContent::Books => "books",
        LibraryContent::Other => "other",
    }
}

/// Inverse of [`content_name`]; values a newer release may write read as
/// `Other`, so a library is never hidden for its type.
fn parse_content(raw: &str) -> LibraryContent {
    match raw {
        "movies" => LibraryContent::Movies,
        "series" => LibraryContent::Series,
        "music" => LibraryContent::Music,
        "books" => LibraryContent::Books,
        _ => LibraryContent::Other,
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    library_id: String,
    grant_id: String,
    name: String,
    content: String,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
    ignored_at: Option<DateTime<Utc>>,
    ignored_by: Option<String>,
}

impl From<Row> for ServiceLibrary {
    fn from(row: Row) -> Self {
        Self {
            library: MediaLibrary {
                id: row.library_id,
                grant_id: Some(row.grant_id),
                name: row.name,
                content: parse_content(&row.content),
            },
            first_seen: row.first_seen,
            last_seen: row.last_seen,
            ignored_at: row.ignored_at,
            ignored_by: row.ignored_by,
        }
    }
}

/// Listed at, complete, error.
type StateRow = (Option<DateTime<Utc>>, Option<bool>, Option<String>);

impl Database {
    /// The libraries of one instance as last listed, by name
    /// (case-insensitive).
    pub async fn service_libraries(
        &self,
        instance_id: i64,
    ) -> Result<Vec<ServiceLibrary>, DbError> {
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT library_id, grant_id, name, content, first_seen, last_seen, ignored_at,
                    ignored_by
             FROM service_library WHERE service_instance_id = ?
             ORDER BY name COLLATE NOCASE, library_id",
        )
        .bind(instance_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Store a successful library listing in one transaction: listed
    /// libraries are inserted or refreshed; a complete listing also removes
    /// the ones it no longer lists (an incomplete one never does). Libraries
    /// without a grant ID cannot be mapped: they are not stored, and the
    /// listing counts as incomplete. `false`, and nothing stored, if the
    /// instance no longer exists.
    pub async fn store_library_listing(
        &self,
        instance_id: i64,
        listing: &Listing<MediaLibrary>,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let grantable: Vec<(&MediaLibrary, &str)> = listing
            .items
            .iter()
            .filter_map(|l| Some((l, l.grant_id.as_deref()?)))
            .collect();
        let complete = listing.complete && grantable.len() == listing.items.len();
        let mut tx = self.begin_write().await?;
        let updated = sqlx::query(
            "UPDATE service_instance SET libraries_listed_at = ?, libraries_complete = ?,
                 libraries_error = NULL
             WHERE id = ?",
        )
        .bind(ts(now))
        .bind(complete)
        .bind(instance_id)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            return Ok(false);
        }
        for (l, grant_id) in &grantable {
            sqlx::query(
                "INSERT INTO service_library (service_instance_id, library_id, grant_id, name,
                     content, first_seen, last_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT (service_instance_id, library_id) DO UPDATE SET
                     grant_id = excluded.grant_id,
                     name = excluded.name,
                     content = excluded.content,
                     last_seen = excluded.last_seen",
            )
            .bind(instance_id)
            .bind(&l.id)
            .bind(grant_id)
            .bind(&l.name)
            .bind(content_name(l.content))
            .bind(ts(now))
            .execute(&mut *tx)
            .await?;
        }
        if complete {
            let listed: Vec<&str> = listing.items.iter().map(|l| l.id.as_str()).collect();
            sqlx::query(
                "DELETE FROM service_library WHERE service_instance_id = ?
                     AND library_id NOT IN (SELECT value FROM json_each(?))",
            )
            .bind(instance_id)
            .bind(serde_json::to_string(&listed)?)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Record a failed library listing (the error code); stored libraries
    /// stay as they are.
    pub async fn record_library_listing_failure(
        &self,
        instance_id: i64,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE service_instance SET libraries_listed_at = ?, libraries_complete = NULL,
                 libraries_error = ?
             WHERE id = ?",
        )
        .bind(ts(now))
        .bind(code)
        .bind(instance_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The last library listing of an instance; `None` if the instance is
    /// unknown.
    pub async fn library_listing_state(
        &self,
        instance_id: i64,
    ) -> Result<Option<LibraryListingState>, DbError> {
        let row: Option<StateRow> = sqlx::query_as(
            "SELECT libraries_listed_at, libraries_complete, libraries_error
             FROM service_instance WHERE id = ?",
        )
        .bind(instance_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|(listed_at, complete, error)| LibraryListingState {
            listed_at,
            complete,
            error,
        }))
    }

    /// Mark a library as belonging to no category, by `admin` (`Some`), or
    /// undo that (`None`). Returns the library as it is now and whether this
    /// call changed it; `None` if the instance has no such library.
    pub async fn set_library_ignored(
        &self,
        instance_id: i64,
        library_id: &str,
        admin: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Option<(ServiceLibrary, bool)>, DbError> {
        let mut tx = self.begin_write().await?;
        let changed = match admin {
            Some(admin) => sqlx::query(
                "UPDATE service_library SET ignored_at = ?, ignored_by = ?
                 WHERE service_instance_id = ? AND library_id = ? AND ignored_at IS NULL",
            )
            .bind(ts(now))
            .bind(admin),
            None => sqlx::query(
                "UPDATE service_library SET ignored_at = NULL, ignored_by = NULL
                 WHERE service_instance_id = ? AND library_id = ? AND ignored_at IS NOT NULL",
            ),
        }
        .bind(instance_id)
        .bind(library_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        let row: Option<Row> = sqlx::query_as(
            "SELECT library_id, grant_id, name, content, first_seen, last_seen, ignored_at,
                    ignored_by
             FROM service_library WHERE service_instance_id = ? AND library_id = ?",
        )
        .bind(instance_id)
        .bind(library_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row.map(|r| (r.into(), changed)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{NewServiceInstance, StoredCredential};
    use crate::test_support::t0;

    async fn instance(db: &Database) -> i64 {
        db.create_service(
            &NewServiceInstance {
                adapter_type: "emby",
                name: "Emby",
                base_url: "http://emby:8096",
                public_url: None,
                credential: &StoredCredential {
                    ciphertext: b"sealed".to_vec(),
                    recipient: "age1test".to_owned(),
                    class: starr_core::adapters::SecretClass::ServiceCredential,
                },
            },
            t0(),
        )
        .await
        .unwrap()
        .id
    }

    fn lib(id: &str, name: &str, content: LibraryContent) -> MediaLibrary {
        MediaLibrary {
            id: id.to_owned(),
            grant_id: Some(format!("g-{id}")),
            name: name.to_owned(),
            content,
        }
    }

    fn listing(items: Vec<MediaLibrary>, complete: bool) -> Listing<MediaLibrary> {
        Listing { items, complete }
    }

    fn names(libs: &[ServiceLibrary]) -> Vec<&str> {
        libs.iter().map(|l| l.library.name.as_str()).collect()
    }

    #[tokio::test]
    async fn listings_round_trip_and_only_complete_ones_remove() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        assert_eq!(
            db.library_listing_state(id).await.unwrap(),
            Some(LibraryListingState {
                listed_at: None,
                complete: None,
                error: None,
            })
        );
        let movies = lib("3", "Movies", LibraryContent::Movies);
        let adult = lib("12", "Adult", LibraryContent::Other);
        assert!(
            db.store_library_listing(
                id,
                &listing(vec![movies.clone(), adult.clone()], true),
                t0()
            )
            .await
            .unwrap()
        );
        let stored = db.service_libraries(id).await.unwrap();
        assert_eq!(names(&stored), vec!["Adult", "Movies"]);
        assert_eq!(stored[1].library, movies);

        // Incomplete without Adult: kept.
        db.store_library_listing(id, &listing(vec![movies.clone()], false), t0())
            .await
            .unwrap();
        assert_eq!(
            names(&db.service_libraries(id).await.unwrap()),
            vec!["Adult", "Movies"]
        );
        assert_eq!(
            db.library_listing_state(id)
                .await
                .unwrap()
                .unwrap()
                .complete,
            Some(false)
        );
        // Complete without Adult: removed.
        db.store_library_listing(id, &listing(vec![movies], true), t0())
            .await
            .unwrap();
        assert_eq!(
            names(&db.service_libraries(id).await.unwrap()),
            vec!["Movies"]
        );

        db.record_library_listing_failure(id, "transient", t0())
            .await
            .unwrap();
        let state = db.library_listing_state(id).await.unwrap().unwrap();
        assert_eq!(state.error.as_deref(), Some("transient"));
        assert_eq!(state.complete, None);
        assert_eq!(db.service_libraries(id).await.unwrap().len(), 1, "kept");
    }

    #[tokio::test]
    async fn libraries_without_a_grant_id_are_not_stored_and_keep_others() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        let movies = lib("3", "Movies", LibraryContent::Movies);
        let adult = lib("12", "Adult", LibraryContent::Other);
        db.store_library_listing(
            id,
            &listing(vec![movies.clone(), adult.clone()], true),
            t0(),
        )
        .await
        .unwrap();
        let no_guid = MediaLibrary {
            grant_id: None,
            ..adult
        };
        db.store_library_listing(id, &listing(vec![movies, no_guid], true), t0())
            .await
            .unwrap();
        // Not stored again, but not removed either: the listing counts as
        // incomplete.
        assert_eq!(
            names(&db.service_libraries(id).await.unwrap()),
            vec!["Adult", "Movies"]
        );
        assert_eq!(
            db.library_listing_state(id)
                .await
                .unwrap()
                .unwrap()
                .complete,
            Some(false)
        );
    }

    #[tokio::test]
    async fn libraries_of_a_removed_instance_are_not_stored() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        db.store_library_listing(
            id,
            &listing(vec![lib("3", "M", LibraryContent::Movies)], true),
            t0(),
        )
        .await
        .unwrap();
        assert!(db.delete_service(id, t0()).await.unwrap());
        assert_eq!(db.service_libraries(id).await.unwrap(), vec![]);
        assert!(
            !db.store_library_listing(id, &listing(vec![], true), t0())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn ignoring_survives_relisting_and_can_be_undone() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        let home = lib("15", "Home videos", LibraryContent::Other);
        db.store_library_listing(id, &listing(vec![home.clone()], true), t0())
            .await
            .unwrap();
        let (row, changed) = db
            .set_library_ignored(id, "15", Some("admin"), t0())
            .await
            .unwrap()
            .unwrap();
        assert!(changed);
        assert_eq!(row.ignored_by.as_deref(), Some("admin"));
        let (_, changed) = db
            .set_library_ignored(id, "15", Some("other"), t0())
            .await
            .unwrap()
            .unwrap();
        assert!(!changed, "already ignored");
        db.store_library_listing(id, &listing(vec![home], true), t0())
            .await
            .unwrap();
        let stored = db.service_libraries(id).await.unwrap();
        assert_eq!(stored[0].ignored_by.as_deref(), Some("admin"));
        let (row, changed) = db
            .set_library_ignored(id, "15", None, t0())
            .await
            .unwrap()
            .unwrap();
        assert!(changed);
        assert_eq!(row.ignored_at, None);
        assert!(
            db.set_library_ignored(id, "nope", None, t0())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn unknown_contents_read_as_other() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        sqlx::query(
            "INSERT INTO service_library (service_instance_id, library_id, grant_id, name, content,
                 first_seen, last_seen)
             VALUES (?, '1', 'g', 'Podcasts', 'podcasts', '2026-11-01T00:00:00.000Z',
                 '2026-11-01T00:00:00.000Z')",
        )
        .bind(id)
        .execute(db.pool())
        .await
        .unwrap();
        let stored = db.service_libraries(id).await.unwrap();
        assert_eq!(stored[0].library.content, LibraryContent::Other);
    }
}
