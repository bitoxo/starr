//! Inventory (domain model §5, spec 0003 slice 6): what each consume
//! service has, by natural key. Decisions come from the pure planner in
//! `starr_core::inventory`; this module only stores and loads rows.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use starr_core::members::Category;
use uuid::Uuid;

use crate::{Database, DbError, ts};

/// One inventory row (item × service instance).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryRow {
    pub natural_key: String,
    /// `movie`, `series` or `album`.
    pub kind: String,
    pub tmdb_id: Option<u32>,
    pub tvdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub remote_id: String,
    pub server_id: Option<String>,
    pub title: String,
    pub year: Option<u32>,
    /// Season number → present episodes (series).
    pub seasons: BTreeMap<u32, u32>,
    pub present_since: Option<DateTime<Utc>>,
    pub last_seen: DateTime<Utc>,
    pub missing_runs: u32,
    pub gone_since: Option<DateTime<Utc>>,
    /// The library IDs the item was listed in; `None` for rows from before
    /// libraries were recorded (until the next sync).
    pub library_ids: Option<Vec<String>>,
    /// Albums: present tracks; `None` for other kinds and while gone.
    pub tracks: Option<u32>,
}

#[derive(sqlx::FromRow)]
struct Row {
    natural_key: String,
    kind: String,
    tmdb_id: Option<i64>,
    tvdb_id: Option<i64>,
    imdb_id: Option<String>,
    remote_id: String,
    server_id: Option<String>,
    title: String,
    year: Option<i64>,
    seasons: String,
    present_since: Option<DateTime<Utc>>,
    last_seen: DateTime<Utc>,
    missing_runs: i64,
    gone_since: Option<DateTime<Utc>>,
    library_ids: Option<String>,
    tracks: Option<i64>,
}

fn small(value: Option<i64>, column: &str) -> Result<Option<u32>, DbError> {
    value
        .map(|v| {
            u32::try_from(v).map_err(|_| DbError::InvalidData(format!("inventory.{column} = {v}")))
        })
        .transpose()
}

impl TryFrom<Row> for InventoryRow {
    type Error = DbError;

    fn try_from(row: Row) -> Result<Self, DbError> {
        Ok(Self {
            tmdb_id: small(row.tmdb_id, "tmdb_id")?,
            tvdb_id: small(row.tvdb_id, "tvdb_id")?,
            year: small(row.year, "year")?,
            // An older binary leaves the count of a row it marks gone.
            tracks: small(row.tracks, "tracks")?.filter(|_| row.present_since.is_some()),
            missing_runs: small(Some(row.missing_runs), "missing_runs")?.unwrap_or(0),
            seasons: serde_json::from_str(&row.seasons)?,
            natural_key: row.natural_key,
            kind: row.kind,
            imdb_id: row.imdb_id,
            remote_id: row.remote_id,
            server_id: row.server_id,
            title: row.title,
            present_since: row.present_since,
            last_seen: row.last_seen,
            gone_since: row.gone_since,
            library_ids: row
                .library_ids
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
        })
    }
}

impl Database {
    /// Whether `category` watches in instance `instance_id` (`consume`
    /// role): `inventory.sync` reads a media server's music libraries only
    /// for the music category.
    pub async fn watches_in(&self, category: Category, instance_id: i64) -> Result<bool, DbError> {
        let (found,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM service_role
             WHERE kind = 'consume' AND category = ? AND service_instance_id = ?)",
        )
        .bind(category.as_str())
        .bind(instance_id)
        .fetch_one(self.pool())
        .await?;
        Ok(found)
    }

    /// Instances that at least one category watches in (`consume` role),
    /// by id: the ones `inventory.sync` reads.
    pub async fn consume_instance_ids(&self) -> Result<Vec<i64>, DbError> {
        let rows: Vec<(i64,)> = sqlx::query_as(
            "SELECT DISTINCT service_instance_id FROM service_role
             WHERE kind = 'consume' ORDER BY service_instance_id",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Every inventory row of one instance, by natural key.
    pub async fn inventory(&self, instance_id: i64) -> Result<Vec<InventoryRow>, DbError> {
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT natural_key, kind, tmdb_id, tvdb_id, imdb_id, remote_id, server_id, title,
                    year, seasons, present_since, last_seen, missing_runs, gone_since, library_ids,
                    tracks
             FROM inventory WHERE service_instance_id = ? ORDER BY natural_key",
        )
        .bind(instance_id)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Store `rows` (insert or replace by natural key) in one transaction.
    /// `false`, and nothing stored, if the instance no longer exists (removed
    /// while its listing ran).
    pub async fn store_inventory(
        &self,
        instance_id: i64,
        rows: &[InventoryRow],
    ) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM service_instance WHERE id = ?")
            .bind(instance_id)
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            return Ok(false);
        }
        for row in rows {
            sqlx::query(
                "INSERT INTO inventory (service_instance_id, natural_key, kind, tmdb_id, tvdb_id,
                     imdb_id, remote_id, server_id, title, year, seasons, present_since, last_seen,
                     missing_runs, gone_since, library_ids, tracks)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (service_instance_id, natural_key) DO UPDATE SET
                     kind = excluded.kind,
                     tmdb_id = excluded.tmdb_id,
                     tvdb_id = excluded.tvdb_id,
                     imdb_id = excluded.imdb_id,
                     remote_id = excluded.remote_id,
                     server_id = excluded.server_id,
                     title = excluded.title,
                     year = excluded.year,
                     seasons = excluded.seasons,
                     present_since = excluded.present_since,
                     last_seen = excluded.last_seen,
                     missing_runs = excluded.missing_runs,
                     gone_since = excluded.gone_since,
                     library_ids = excluded.library_ids,
                     tracks = excluded.tracks",
            )
            .bind(instance_id)
            .bind(&row.natural_key)
            .bind(&row.kind)
            .bind(row.tmdb_id)
            .bind(row.tvdb_id)
            .bind(&row.imdb_id)
            .bind(&row.remote_id)
            .bind(&row.server_id)
            .bind(&row.title)
            .bind(row.year)
            .bind(serde_json::to_string(&row.seasons)?)
            .bind(row.present_since.map(ts))
            .bind(ts(row.last_seen))
            .bind(row.missing_runs)
            .bind(row.gone_since.map(ts))
            .bind(
                row.library_ids
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
            )
            .bind(row.tracks)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }
}

/// Where a title can be opened: one present inventory row in a consume
/// instance of a category, with what an "open in" link is built from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleLink {
    pub service_instance_id: i64,
    pub adapter_type: String,
    pub service_name: String,
    /// The instance's public URL. Instances without one give no links: the
    /// base URL is an internal address and never shown to members
    /// (maintainer, 2026-11-02).
    pub web_url: String,
    pub server_id: Option<String>,
    pub remote_id: String,
}

/// Instance id, adapter type, name, web URL, server id, remote id, and the
/// natural key that picked the row.
type LinkRow = (i64, String, String, String, Option<String>, String, String);

/// The album's natural key, then instance id, adapter type, name, web URL,
/// server id and remote id.
type AlbumLinkRow = (String, i64, String, String, String, Option<String>, String);

/// Catalog item id, then the columns of a [`LinkRow`].
type TitlesLinkRow = (
    i64,
    i64,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
);

impl Database {
    /// Present rows of a title (by any natural key) in the consume
    /// instances of `category` that have a public URL, one per instance, for
    /// "open in" links. `kind` is `movie` or `series`.
    #[allow(clippy::similar_names)] // the three natural keys
    pub async fn title_links(
        &self,
        category: Category,
        kind: &str,
        tmdb_id: Option<u64>,
        tvdb_id: Option<u64>,
        imdb_id: Option<&str>,
    ) -> Result<Vec<TitleLink>, DbError> {
        let to_i64 = |v: Option<u64>| v.and_then(|v| i64::try_from(v).ok());
        let rows: Vec<LinkRow> = sqlx::query_as(concat!(
            // One row per instance; the bare columns come from the row with
            // the smallest natural key (SQLite's min() aggregate rule), so
            // server and remote id always belong together.
            "SELECT s.id, s.adapter_type, s.name, s.public_url,
                    i.server_id, i.remote_id, min(i.natural_key)
             FROM inventory i
             JOIN service_role r ON r.service_instance_id = i.service_instance_id
                  AND r.kind = 'consume' AND r.category = ?1
             JOIN service_instance s ON s.id = i.service_instance_id
             WHERE i.kind = ?2 AND i.present_since IS NOT NULL AND s.public_url IS NOT NULL
               AND ((?3 IS NOT NULL AND i.tmdb_id = ?3) OR (?4 IS NOT NULL AND i.tvdb_id = ?4)
                    OR (?5 IS NOT NULL AND i.imdb_id = ?5))
               AND ",
            in_role_libraries!("r", "i"),
            " GROUP BY s.id ORDER BY s.name, s.id"
        ))
        .bind(category.as_str())
        .bind(kind)
        .bind(to_i64(tmdb_id))
        .bind(to_i64(tvdb_id))
        .bind(imdb_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    service_instance_id,
                    adapter_type,
                    service_name,
                    web_url,
                    server_id,
                    remote_id,
                    _,
                )| {
                    TitleLink {
                        service_instance_id,
                        adapter_type,
                        service_name,
                        web_url,
                        server_id,
                        remote_id,
                    }
                },
            )
            .collect())
    }

    /// The libraries of `category` as "open in" targets: one per library a
    /// Jellyfin or Emby consume role of the category maps, on instances
    /// with a public URL (spec 0007 Q5 A: scenes open the member's adult
    /// library, not the item). `remote_id` is the library's id;
    /// `service_name` names the instance and the library.
    pub async fn category_library_links(
        &self,
        category: Category,
    ) -> Result<Vec<TitleLink>, DbError> {
        let rows: Vec<(i64, String, String, Option<String>, String, String)> = sqlx::query_as(
            "SELECT s.id, s.adapter_type, s.name, l.name, s.public_url,
                    json_extract(rl.value, '$.id')
             FROM service_role r
             JOIN service_instance s ON s.id = r.service_instance_id
                  AND s.public_url IS NOT NULL AND s.adapter_type IN ('jellyfin', 'emby')
             JOIN json_each(r.settings, '$.libraries') rl
             LEFT JOIN service_library l ON l.service_instance_id = s.id
                  AND l.library_id = json_extract(rl.value, '$.id')
             WHERE r.kind = 'consume' AND r.category = ?
               AND coalesce(json_extract(rl.value, '$.id'), '') <> ''
             ORDER BY s.name, s.id, l.name",
        )
        .bind(category.as_str())
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(service_instance_id, adapter_type, instance, library, web_url, remote_id)| {
                    TitleLink {
                        service_instance_id,
                        adapter_type,
                        service_name: match library {
                            Some(library) => format!("{instance} · {library}"),
                            None => instance,
                        },
                        web_url,
                        server_id: None,
                        remote_id,
                    }
                },
            )
            .collect())
    }

    /// [`Database::title_links`] for many catalog items (movies or series)
    /// at once, each in its category, by catalog item id; items without
    /// links are left out.
    pub async fn titles_links(
        &self,
        titles: &[(i64, Category)],
    ) -> Result<BTreeMap<i64, Vec<TitleLink>>, DbError> {
        if titles.is_empty() {
            return Ok(BTreeMap::new());
        }
        let wanted: Vec<serde_json::Value> = titles
            .iter()
            .map(|(id, category)| serde_json::json!({ "id": id, "category": category.as_str() }))
            .collect();
        let rows: Vec<TitlesLinkRow> = sqlx::query_as(concat!(
            // As in `title_links`, per title and instance.
            "SELECT c.id, s.id, s.adapter_type, s.name, s.public_url,
                        i.server_id, i.remote_id, min(i.natural_key)
                 FROM json_each(?1) w
                 JOIN catalog_item c ON c.id = json_extract(w.value, '$.id')
                 JOIN service_role r ON r.kind = 'consume'
                      AND r.category = json_extract(w.value, '$.category')
                 JOIN service_instance s ON s.id = r.service_instance_id
                      AND s.public_url IS NOT NULL
                 JOIN inventory i ON i.service_instance_id = s.id AND i.kind = c.kind
                      AND i.present_since IS NOT NULL
                      AND (i.tmdb_id = ",
            tmdb_key!("c.id"),
            " OR i.tvdb_id = ",
            tvdb_key!("c.id"),
            " OR i.imdb_id = ",
            imdb_key!("c.id"),
            ") AND ",
            in_role_libraries!("r", "i"),
            "
                 GROUP BY c.id, s.id ORDER BY c.id, s.name, s.id"
        ))
        .bind(serde_json::to_string(&wanted)?)
        .fetch_all(self.pool())
        .await?;
        let mut out: BTreeMap<i64, Vec<TitleLink>> = BTreeMap::new();
        for (
            title,
            service_instance_id,
            adapter_type,
            service_name,
            web_url,
            server_id,
            remote_id,
            _,
        ) in rows
        {
            out.entry(title).or_default().push(TitleLink {
                service_instance_id,
                adapter_type,
                service_name,
                web_url,
                server_id,
                remote_id,
            });
        }
        Ok(out)
    }

    /// "Open in" links of albums (spec 0006 slice 5): present album rows
    /// with these release groups in the music consume instances that have
    /// a public URL, one per album and instance, by release group.
    pub async fn album_links(
        &self,
        release_groups: &[Uuid],
    ) -> Result<BTreeMap<Uuid, Vec<TitleLink>>, DbError> {
        if release_groups.is_empty() {
            return Ok(BTreeMap::new());
        }
        let keys: Vec<String> = release_groups.iter().map(|rg| album_key(*rg)).collect();
        let rows: Vec<AlbumLinkRow> = sqlx::query_as(concat!(
            // As in `title_links`, per album and instance.
            "SELECT min(i.natural_key), s.id, s.adapter_type, s.name, s.public_url,
                        i.server_id, i.remote_id
                 FROM inventory i
                 JOIN service_role r ON r.service_instance_id = i.service_instance_id
                      AND r.kind = 'consume' AND r.category = 'music'
                 JOIN service_instance s ON s.id = i.service_instance_id
                 WHERE i.kind = 'album' AND i.present_since IS NOT NULL
                   AND s.public_url IS NOT NULL
                   AND i.natural_key IN (SELECT value FROM json_each(?1))
                   AND ",
            in_role_libraries!("r", "i"),
            " GROUP BY i.natural_key, s.id ORDER BY i.natural_key, s.name, s.id"
        ))
        .bind(serde_json::to_string(&keys)?)
        .fetch_all(self.pool())
        .await?;
        let mut out: BTreeMap<Uuid, Vec<TitleLink>> = BTreeMap::new();
        for (key, service_instance_id, adapter_type, service_name, web_url, server_id, remote_id) in
            rows
        {
            let Some(rg) = release_group_of(&key) else {
                continue;
            };
            out.entry(rg).or_default().push(TitleLink {
                service_instance_id,
                adapter_type,
                service_name,
                web_url,
                server_id,
                remote_id,
            });
        }
        Ok(out)
    }
}

/// The inventory key of an album (ADR 0013): `album:mbrg:<uuid>`.
#[must_use]
pub fn album_key(release_group: Uuid) -> String {
    format!("album:mbrg:{}", release_group.hyphenated())
}

/// The release group of an album's inventory key.
#[must_use]
pub fn release_group_of(key: &str) -> Option<Uuid> {
    key.strip_prefix("album:mbrg:")
        .and_then(|id| Uuid::try_parse(id).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{NewServiceInstance, StoredCredential};
    use crate::test_support::t0;

    async fn instance(db: &Database) -> i64 {
        db.create_service(
            &NewServiceInstance {
                adapter_type: "jellyfin",
                name: "Jellyfin",
                base_url: "http://jellyfin:8096",
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

    fn row(key: &str) -> InventoryRow {
        InventoryRow {
            natural_key: key.to_owned(),
            kind: "series".to_owned(),
            tmdb_id: Some(1396),
            tvdb_id: Some(81189),
            imdb_id: Some("tt0903747".to_owned()),
            remote_id: "abc".to_owned(),
            server_id: Some("srv".to_owned()),
            title: "Breaking Bad".to_owned(),
            year: Some(2008),
            seasons: BTreeMap::from([(1, 7), (2, 13)]),
            present_since: Some(t0()),
            last_seen: t0(),
            missing_runs: 0,
            gone_since: None,
            library_ids: Some(vec!["6".to_owned()]),
            tracks: None,
        }
    }

    #[tokio::test]
    async fn rows_round_trip_and_update_in_place() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        let first = row("series:tvdb:81189");
        assert!(
            db.store_inventory(id, std::slice::from_ref(&first))
                .await
                .unwrap()
        );
        assert_eq!(db.inventory(id).await.unwrap(), vec![first.clone()]);

        let gone = InventoryRow {
            present_since: None,
            gone_since: Some(t0()),
            missing_runs: 2,
            seasons: BTreeMap::new(),
            ..first
        };
        db.store_inventory(id, std::slice::from_ref(&gone))
            .await
            .unwrap();
        assert_eq!(db.inventory(id).await.unwrap(), vec![gone]);
    }

    #[tokio::test]
    async fn rows_of_a_removed_instance_are_not_stored_and_go_with_it() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        db.store_inventory(id, &[row("series:tvdb:1")])
            .await
            .unwrap();
        assert!(db.delete_service(id, t0()).await.unwrap());
        assert_eq!(db.inventory(id).await.unwrap(), vec![]);
        assert!(!db.store_inventory(id, &[row("x")]).await.unwrap());
    }

    #[tokio::test]
    async fn consume_instances_are_those_with_a_consume_role() {
        use crate::categories::NewRole;
        use starr_core::adapters::Capability;
        use starr_core::categories::RoleKind;
        use starr_core::members::Category;

        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        assert_eq!(db.consume_instance_ids().await.unwrap(), Vec::<i64>::new());
        for category in [Category::Movies, Category::Series] {
            db.add_role(
                &NewRole {
                    category,
                    kind: RoleKind::Consume,
                    service_instance_id: id,
                    offered: &[Capability::Libraries],
                },
                t0(),
            )
            .await
            .unwrap();
        }
        assert_eq!(db.consume_instance_ids().await.unwrap(), vec![id]);
    }

    #[tokio::test]
    async fn present_and_gone_are_exclusive() {
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        let both = InventoryRow {
            gone_since: Some(t0()),
            ..row("series:tvdb:1")
        };
        assert!(db.store_inventory(id, &[both]).await.is_err());
        assert_eq!(db.inventory(id).await.unwrap(), vec![]);
    }

    #[tokio::test]
    async fn links_come_from_present_rows_of_the_category_consume_instances() {
        use crate::categories::NewRole;
        use crate::services::ServiceUpdate;
        use starr_core::adapters::Capability;
        use starr_core::categories::RoleKind;
        use starr_core::members::Category;

        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        db.store_inventory(id, &[row("series:tvdb:81189")])
            .await
            .unwrap();
        let find = |db: Database| async move {
            db.title_links(Category::Series, "series", Some(1396), None, None)
                .await
                .unwrap()
        };
        // Not watched in by the category: no links.
        assert_eq!(find(db.clone()).await, vec![]);
        db.add_role(
            &NewRole {
                category: Category::Series,
                kind: RoleKind::Consume,
                service_instance_id: id,
                offered: &[Capability::Libraries],
            },
            t0(),
        )
        .await
        .unwrap();
        // Without a public URL: no link, the internal base URL never leaks.
        assert_eq!(find(db.clone()).await, vec![]);
        db.update_service(
            id,
            &ServiceUpdate {
                name: None,
                base_url: None,
                public_url: Some(Some("https://watch.example.com")),
                credential: None,
            },
            t0(),
        )
        .await
        .unwrap();
        assert_eq!(
            find(db.clone()).await,
            vec![TitleLink {
                service_instance_id: id,
                adapter_type: "jellyfin".to_owned(),
                service_name: "Jellyfin".to_owned(),
                web_url: "https://watch.example.com".to_owned(),
                server_id: Some("srv".to_owned()),
                remote_id: "abc".to_owned(),
            }]
        );
        // Another category or kind, or another title: nothing.
        assert_eq!(
            db.title_links(Category::Movies, "series", Some(1396), None, None)
                .await
                .unwrap(),
            vec![]
        );
        assert_eq!(
            db.title_links(Category::Series, "movie", Some(1396), None, None)
                .await
                .unwrap(),
            vec![]
        );
        // Many titles at once (the member's requests): the same links, by
        // catalog item, only for the category's consume instances.
        let (series_id,): (i64,) = sqlx::query_as(
            "INSERT INTO catalog_item (kind, tmdb_id, title, created_at, updated_at)
             VALUES ('series', 1396, 'Breaking Bad', '2026-11-01', '2026-11-01') RETURNING id",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        crate::test_support::keys_from_columns(&db).await;
        let many = db
            .titles_links(&[(series_id, Category::Series), (999, Category::Series)])
            .await
            .unwrap();
        assert_eq!(many.get(&series_id), Some(&find(db.clone()).await));
        assert_eq!(many.len(), 1);
        assert!(
            db.titles_links(&[(series_id, Category::Movies)])
                .await
                .unwrap()
                .is_empty()
        );
        assert!(db.titles_links(&[]).await.unwrap().is_empty());
        // Matched by any natural key; gone rows link nowhere.
        assert_eq!(
            db.title_links(Category::Series, "series", None, None, Some("tt0903747"))
                .await
                .unwrap()
                .len(),
            1
        );
        let gone = InventoryRow {
            present_since: None,
            gone_since: Some(t0()),
            ..row("series:tvdb:81189")
        };
        db.store_inventory(id, &[gone]).await.unwrap();
        assert_eq!(find(db.clone()).await, vec![]);
    }

    /// One Emby is consume for movies and adult. "Matrix" is in the movies
    /// library, "Adult film" (also with a TMDB ID) in the adult library.
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one availability story
    async fn availability_counts_only_the_category_libraries() {
        use crate::catalog::ItemKind;
        use crate::categories::NewRole;
        use crate::requests::TitlePresence;
        use starr_core::adapters::Capability;
        use starr_core::categories::RoleKind;
        use starr_core::libraries::{ConsumeSettings, MappedLibrary};

        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        let mut roles = std::collections::HashMap::new();
        for category in [Category::Movies, Category::Adult] {
            let role = db
                .add_role(
                    &NewRole {
                        category,
                        kind: RoleKind::Consume,
                        service_instance_id: id,
                        offered: &[Capability::Libraries],
                    },
                    t0(),
                )
                .await
                .unwrap();
            roles.insert(category, role.id);
        }
        let movie = |key: &str, tmdb: u32, library: Option<&str>| InventoryRow {
            natural_key: key.to_owned(),
            kind: "movie".to_owned(),
            tmdb_id: Some(tmdb),
            tvdb_id: None,
            imdb_id: None,
            seasons: BTreeMap::new(),
            library_ids: library.map(|l| vec![l.to_owned()]),
            ..row(key)
        };
        db.store_inventory(
            id,
            &[
                movie("movie:tmdb:603", 603, Some("3")),
                movie("movie:tmdb:9001", 9001, Some("12")),
                // Written before libraries were recorded.
                movie("movie:tmdb:7", 7, None),
                // In the adult library and in the movies library.
                InventoryRow {
                    library_ids: Some(vec!["12".to_owned(), "3".to_owned()]),
                    ..movie("movie:tmdb:42", 42, None)
                },
                // Listed in no library (an empty list).
                InventoryRow {
                    library_ids: Some(vec![]),
                    ..movie("movie:tmdb:43", 43, None)
                },
            ],
        )
        .await
        .unwrap();
        let present = |db: Database, category: Category, tmdb: u64| async move {
            matches!(
                db.title_presence(category, ItemKind::Movie, Some(tmdb), None, None)
                    .await
                    .unwrap(),
                TitlePresence::Present(_)
            )
        };
        let map = |db: Database, category: Category, ids: &'static [&'static str]| {
            let role = roles[&category];
            async move {
                let settings = ConsumeSettings {
                    libraries: ids
                        .iter()
                        .map(|id| MappedLibrary {
                            id: (*id).to_owned(),
                            grant_id: format!("g-{id}"),
                        })
                        .collect(),
                };
                db.set_consume_libraries(role, &settings, t0())
                    .await
                    .unwrap()
                    .unwrap();
            }
        };

        // No mapping yet: today's behaviour, everything counts.
        for tmdb in [603, 9001, 7] {
            assert!(present(db.clone(), Category::Movies, tmdb).await, "{tmdb}");
        }

        // Adult maps its library: the adult film no longer counts as a
        // movie, although movies has no mapping yet.
        map(db.clone(), Category::Adult, &["12"]).await;
        assert!(present(db.clone(), Category::Movies, 603).await);
        assert!(!present(db.clone(), Category::Movies, 9001).await);
        assert!(
            present(db.clone(), Category::Movies, 7).await,
            "unknown library"
        );
        assert!(present(db.clone(), Category::Adult, 9001).await);
        assert!(!present(db.clone(), Category::Adult, 603).await);
        // Also in a library nobody mapped: counts for unmapped movies.
        assert!(present(db.clone(), Category::Movies, 42).await);
        assert!(present(db.clone(), Category::Adult, 42).await);
        assert!(
            present(db.clone(), Category::Movies, 43).await,
            "empty list"
        );

        // Movies cannot take the adult library (checked in the write).
        let taken = db
            .set_consume_libraries(
                roles[&Category::Movies],
                &ConsumeSettings {
                    libraries: vec![MappedLibrary {
                        id: "12".to_owned(),
                        grant_id: "g-12".to_owned(),
                    }],
                },
                t0(),
            )
            .await;
        assert!(matches!(taken, Err(DbError::LibraryTaken)), "{taken:?}");

        // Movies maps its library: only its items count.
        map(db.clone(), Category::Movies, &["3"]).await;
        assert!(present(db.clone(), Category::Movies, 603).await);
        assert!(!present(db.clone(), Category::Movies, 9001).await);
        assert!(!present(db.clone(), Category::Movies, 7).await);
        assert!(present(db.clone(), Category::Movies, 42).await, "in both");
        assert!(!present(db.clone(), Category::Movies, 43).await);

        // Links follow the same rule.
        db.update_service(
            id,
            &crate::services::ServiceUpdate {
                name: None,
                base_url: None,
                public_url: Some(Some("https://watch.example.com")),
                credential: None,
            },
            t0(),
        )
        .await
        .unwrap();
        assert_eq!(
            db.title_links(Category::Movies, "movie", Some(9001), None, None)
                .await
                .unwrap(),
            vec![]
        );
        assert_eq!(
            db.title_links(Category::Adult, "movie", Some(9001), None, None)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// Navidrome is music's consume service: albums count by release group
    /// only, gone ones and other categories' services never.
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one presence story
    async fn albums_are_present_by_release_group_with_their_tracks() {
        use crate::categories::NewRole;
        use crate::requests::TitlePresence;
        use crate::services::ServiceUpdate;
        use starr_core::adapters::Capability;
        use starr_core::categories::RoleKind;
        use starr_core::requests::ConsumePresence;

        let ok_computer = Uuid::from_u128(0xb139_2450_e666_3926_a536_22c6_5f83_4433);
        let kid_a = Uuid::from_u128(0x1b02_2e01_4da6_387b_8658_8678_046e_4cef);
        let unknown = Uuid::from_u128(7);
        let db = Database::open_in_memory().await.unwrap();
        let id = instance(&db).await;
        let album = |rg: Uuid, tracks: u32| InventoryRow {
            natural_key: album_key(rg),
            kind: "album".to_owned(),
            tmdb_id: None,
            tvdb_id: None,
            imdb_id: None,
            remote_id: format!("nd-{tracks}"),
            server_id: None,
            seasons: BTreeMap::new(),
            tracks: Some(tracks),
            ..row("x")
        };
        let gone = InventoryRow {
            present_since: None,
            gone_since: Some(t0()),
            ..album(kid_a, 9)
        };
        db.store_inventory(id, &[album(ok_computer, 12), gone])
            .await
            .unwrap();
        let stored = db.inventory(id).await.unwrap();
        assert_eq!(stored[0].tracks, None, "gone rows have no tracks");
        assert_eq!(stored[1].tracks, Some(12));
        assert_eq!(stored[1].seasons, BTreeMap::new());
        // A movie row cannot carry tracks, an album needs its album key.
        let bad = InventoryRow {
            tracks: Some(1),
            ..row("movie:tmdb:1")
        };
        assert!(db.store_inventory(id, &[bad]).await.is_err());
        let bad = InventoryRow {
            natural_key: "album:name:ok computer".to_owned(),
            ..album(ok_computer, 1)
        };
        assert!(db.store_inventory(id, &[bad]).await.is_err());

        let all = [ok_computer, kid_a, unknown];
        // No music consume role yet: the source's files decide.
        assert_eq!(
            db.album_presence(Category::Music, &all).await.unwrap(),
            TitlePresence::NoService
        );
        db.add_role(
            &NewRole {
                category: Category::Music,
                kind: RoleKind::Consume,
                service_instance_id: id,
                offered: &[Capability::Libraries],
            },
            t0(),
        )
        .await
        .unwrap();
        let presence = db.album_presence(Category::Music, &all).await.unwrap();
        assert_eq!(
            presence,
            TitlePresence::Albums(BTreeMap::from([(ok_computer, 12)]))
        );
        assert_eq!(
            presence.unit(None, Some(ok_computer)),
            ConsumePresence::Present { episodes: 12 }
        );
        assert_eq!(presence.unit(None, Some(kid_a)), ConsumePresence::Absent);
        assert_eq!(
            db.album_presence(Category::Music, &[]).await.unwrap(),
            TitlePresence::Albums(BTreeMap::new())
        );

        // The same server also holds movies for the movies category: its
        // movies count as before, its albums never as movies.
        db.add_role(
            &NewRole {
                category: Category::Movies,
                kind: RoleKind::Consume,
                service_instance_id: id,
                offered: &[Capability::Libraries],
            },
            t0(),
        )
        .await
        .unwrap();
        let movie = InventoryRow {
            kind: "movie".to_owned(),
            tvdb_id: None,
            imdb_id: None,
            seasons: BTreeMap::new(),
            ..row("movie:tmdb:1396")
        };
        db.store_inventory(id, &[movie]).await.unwrap();
        let movies = |tmdb: u64| {
            let db = db.clone();
            async move {
                db.title_presence(
                    Category::Movies,
                    crate::catalog::ItemKind::Movie,
                    Some(tmdb),
                    None,
                    None,
                )
                .await
                .unwrap()
            }
        };
        assert!(matches!(movies(1396).await, TitlePresence::Present(_)));
        assert_eq!(movies(12).await, TitlePresence::Absent);
        assert_eq!(
            db.album_presence(Category::Music, &all).await.unwrap(),
            TitlePresence::Albums(BTreeMap::from([(ok_computer, 12)])),
            "the movie row is no album"
        );

        // Links need the public URL.
        assert!(db.album_links(&all).await.unwrap().is_empty());
        db.update_service(
            id,
            &ServiceUpdate {
                name: None,
                base_url: None,
                public_url: Some(Some("https://music.example.com")),
                credential: None,
            },
            t0(),
        )
        .await
        .unwrap();
        let links = db.album_links(&all).await.unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[&ok_computer][0].remote_id, "nd-12");
        assert_eq!(links[&ok_computer][0].web_url, "https://music.example.com");
        assert!(db.album_links(&[]).await.unwrap().is_empty());
    }

    #[test]
    fn album_keys_round_trip() {
        use starr_core::library::{InventoryKey, ItemKind, ProviderIds};
        let rg = Uuid::from_u128(0x1b02_2e01_4da6_387b_8658_8678_046e_4cef);
        assert_eq!(
            album_key(rg),
            "album:mbrg:1b022e01-4da6-387b-8658-8678046e4cef"
        );
        assert_eq!(release_group_of(&album_key(rg)), Some(rg));
        assert_eq!(release_group_of("movie:tmdb:1"), None);
        let ids = ProviderIds {
            mb_release_group: Some(rg),
            ..ProviderIds::default()
        };
        assert_eq!(
            InventoryKey::of(ItemKind::Album, &ids).map(|k| k.to_string()),
            Some(album_key(rg)),
            "the worker's key and the lookups' key agree"
        );
    }
}
