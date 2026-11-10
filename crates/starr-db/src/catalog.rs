//! Catalog items and external references (domain model §5, spec 0003
//! slice 2). Shared: no member owns a catalog item; requests (slice 4) point
//! at them.
//!
//! Identity follows lesson 7: an item has Starr's own id and natural keys
//! (TMDB, TVDB, `IMDb`, `MusicBrainz`, `StashDB`, …) in `catalog_key`, one
//! per scheme (ADR 0012). Remote ids a service uses (external references)
//! are a cache that may go stale; the natural key is the fallback.
//!
//! Expand/contract (ADR 0008): until the contract release the TMDB, TVDB
//! and `IMDb` keys are also written to the old columns of `catalog_item`,
//! but every read goes to `catalog_key`.

use chrono::{DateTime, Datelike, TimeDelta, Utc};
use serde_json::json;
use starr_core::catalog::{CatalogDetails, CatalogEntry, ExternalId, MediaKind, Season};

use crate::{Database, DbError, ts};

/// Metadata from a catalog is kept at most this long (TMDB terms: six
/// months). The schema refuses longer expiries.
pub const METADATA_MAX_AGE: TimeDelta = TimeDelta::days(183);

/// What a catalog item is (ADR 0012). Seasons and albums hang off a
/// parent (series, artist); only seasons have a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    Movie,
    Series,
    Season,
    Artist,
    Album,
    Scene,
    Performer,
    Studio,
}

impl ItemKind {
    pub const ALL: [Self; 8] = [
        Self::Movie,
        Self::Series,
        Self::Season,
        Self::Artist,
        Self::Album,
        Self::Scene,
        Self::Performer,
        Self::Studio,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Series => "series",
            Self::Season => "season",
            Self::Artist => "artist",
            Self::Album => "album",
            Self::Scene => "scene",
            Self::Performer => "performer",
            Self::Studio => "studio",
        }
    }

    /// The kind stored as `raw`; [`DbError::UnknownValue`] for a kind a
    /// newer release added.
    pub(crate) fn parse(raw: &str) -> Result<Self, DbError> {
        Self::ALL
            .into_iter()
            .find(|k| k.as_str() == raw)
            .ok_or_else(|| crate::unknown("item kind", raw))
    }
}

impl From<MediaKind> for ItemKind {
    fn from(kind: MediaKind) -> Self {
        match kind {
            MediaKind::Movie => Self::Movie,
            MediaKind::Series => Self::Series,
            MediaKind::Artist => Self::Artist,
            MediaKind::Album => Self::Album,
            MediaKind::Scene => Self::Scene,
            MediaKind::Performer => Self::Performer,
            MediaKind::Studio => Self::Studio,
        }
    }
}

/// A movie, series or season.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogItem {
    pub id: i64,
    pub kind: ItemKind,
    /// Seasons: the series.
    pub parent_id: Option<i64>,
    pub season_number: Option<u32>,
    pub tmdb_id: Option<u64>,
    pub tvdb_id: Option<u64>,
    pub imdb_id: Option<String>,
    pub title: String,
    pub year: Option<i32>,
    /// Image path for the image proxy.
    pub poster_path: Option<String>,
    /// Cached metadata (JSON), with provenance and expiry below.
    pub metadata: Option<String>,
    /// Adapter type the metadata came from, e.g. `tmdb`.
    pub metadata_source: Option<String>,
    pub metadata_instance_id: Option<i64>,
    pub metadata_fetched_at: Option<DateTime<Utc>>,
    pub metadata_expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Where metadata comes from and how long it may be kept.
#[derive(Debug, Clone, Copy)]
pub struct Provenance<'a> {
    /// Adapter type, e.g. `tmdb`.
    pub adapter_type: &'a str,
    pub instance_id: i64,
    pub fetched_at: DateTime<Utc>,
    /// Capped at [`METADATA_MAX_AGE`] after `fetched_at`.
    pub expires_at: DateTime<Utc>,
}

/// The id a service uses for an item, and when that was last confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRef {
    pub catalog_item_id: i64,
    pub service_instance_id: i64,
    pub remote_id: String,
    pub confirmed_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct ItemRow {
    id: i64,
    kind: String,
    parent_id: Option<i64>,
    season_number: Option<i64>,
    tmdb_id: Option<i64>,
    tvdb_id: Option<i64>,
    imdb_id: Option<String>,
    title: String,
    year: Option<i64>,
    poster_path: Option<String>,
    metadata: Option<String>,
    metadata_source: Option<String>,
    metadata_instance_id: Option<i64>,
    metadata_fetched_at: Option<DateTime<Utc>>,
    metadata_expires_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

macro_rules! item_columns {
    () => {
        concat!(
            "id, kind, parent_id, season_number, ",
            tmdb_key!("catalog_item.id"),
            " AS tmdb_id, ",
            tvdb_key!("catalog_item.id"),
            " AS tvdb_id, ",
            imdb_key!("catalog_item.id"),
            " AS imdb_id, title, year, poster_path, metadata, metadata_source, \
             metadata_instance_id, metadata_fetched_at, metadata_expires_at, created_at, \
             updated_at"
        )
    };
}

fn to_u64(v: Option<i64>) -> Option<u64> {
    v.and_then(|v| u64::try_from(v).ok())
}

impl TryFrom<ItemRow> for CatalogItem {
    type Error = DbError;

    fn try_from(row: ItemRow) -> Result<Self, DbError> {
        Ok(Self {
            id: row.id,
            kind: ItemKind::parse(&row.kind)?,
            parent_id: row.parent_id,
            season_number: row.season_number.and_then(|n| u32::try_from(n).ok()),
            tmdb_id: to_u64(row.tmdb_id),
            tvdb_id: to_u64(row.tvdb_id),
            imdb_id: row.imdb_id,
            title: row.title,
            year: row.year.and_then(|y| i32::try_from(y).ok()),
            poster_path: row.poster_path,
            metadata: row.metadata,
            metadata_source: row.metadata_source,
            metadata_instance_id: row.metadata_instance_id,
            metadata_fetched_at: row.metadata_fetched_at,
            metadata_expires_at: row.metadata_expires_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

/// A natural key as `catalog_key` stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    scheme: &'static str,
    value: String,
}

/// The `catalog_key` scheme and value of `id` on an item of `kind`; `None`
/// when the id does not name that kind (a TVDB id on a movie, an MBID on a
/// series) or is malformed. TVDB ids are series ids (ADR 0012).
fn key_of(kind: ItemKind, id: &ExternalId) -> Option<Key> {
    let in_range = |v: u64| v > 0 && i64::try_from(v).is_ok();
    let (scheme, value) = match (kind, id) {
        // Ids must fit the integer key columns (and SQLite's integers).
        (ItemKind::Movie, ExternalId::Tmdb(v)) if in_range(*v) => ("tmdb_movie", v.to_string()),
        (ItemKind::Series, ExternalId::Tmdb(v)) if in_range(*v) => ("tmdb_tv", v.to_string()),
        (ItemKind::Series, ExternalId::Tvdb(v)) if in_range(*v) => ("tvdb", v.to_string()),
        (ItemKind::Movie | ItemKind::Series, ExternalId::Imdb(v))
            if v.len() > 2 && v.starts_with("tt") && v[2..].bytes().all(|b| b.is_ascii_digit()) =>
        {
            ("imdb", v.clone())
        }
        (ItemKind::Artist, ExternalId::MbArtist(u)) => ("mb_artist", u.to_string()),
        (ItemKind::Album, ExternalId::MbReleaseGroup(u)) => ("mb_release_group", u.to_string()),
        (ItemKind::Scene, ExternalId::StashScene(u)) => ("stashdb_scene", u.to_string()),
        (ItemKind::Performer, ExternalId::StashPerformer(u)) => {
            ("stashdb_performer", u.to_string())
        }
        (ItemKind::Studio, ExternalId::StashStudio(u)) => ("stashdb_studio", u.to_string()),
        _ => return None,
    };
    Some(Key { scheme, value })
}

/// Natural keys of one title of `kind`, at most one per scheme (the first
/// wins), TMDB first, then TVDB, then `IMDb`, then the others: the order
/// [`find_by_keys`] tries them in.
#[derive(Debug, Default)]
struct Keys(Vec<Key>);

impl Keys {
    fn from_ids(kind: ItemKind, ids: &[ExternalId]) -> Self {
        let rank = |scheme: &str| match scheme {
            "tmdb_movie" | "tmdb_tv" => 0,
            "tvdb" => 1,
            "imdb" => 2,
            _ => 3,
        };
        let mut keys: Vec<Key> = Vec::new();
        for key in ids.iter().filter_map(|id| key_of(kind, id)) {
            if !keys.iter().any(|k| k.scheme == key.scheme) {
                keys.push(key);
            }
        }
        keys.sort_by_key(|k| rank(k.scheme));
        Self(keys)
    }

    fn value(&self, schemes: &[&str]) -> Option<&str> {
        self.0
            .iter()
            .find(|k| schemes.contains(&k.scheme))
            .map(|k| k.value.as_str())
    }

    /// The old key columns, still written until the contract release.
    fn tmdb_column(&self) -> Option<i64> {
        self.value(&["tmdb_movie", "tmdb_tv"])
            .and_then(|v| v.parse().ok())
    }

    fn tvdb_column(&self) -> Option<i64> {
        self.value(&["tvdb"]).and_then(|v| v.parse().ok())
    }

    fn imdb_column(&self) -> Option<&str> {
        self.value(&["imdb"])
    }
}

/// Find an item of `kind` by any natural key, trying them in order.
async fn find_by_keys(
    conn: &mut sqlx::SqliteConnection,
    kind: ItemKind,
    keys: &Keys,
) -> Result<Option<i64>, DbError> {
    for key in &keys.0 {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT k.catalog_item_id FROM catalog_key k
             JOIN catalog_item i ON i.id = k.catalog_item_id
             WHERE k.scheme = ? AND k.value = ? AND i.kind = ?",
        )
        .bind(key.scheme)
        .bind(&key.value)
        .bind(kind.as_str())
        .fetch_optional(&mut *conn)
        .await?;
        if let Some((id,)) = row {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Drop keys another item already holds (two catalog entries claiming one
/// TVDB id, say): the item keeps the keys it can have instead of failing
/// the whole upsert. A key names one item of any kind. Until the contract
/// release the old key columns (unique per kind) count too, since they are
/// still written.
async fn drop_taken_keys(
    conn: &mut sqlx::SqliteConnection,
    kind: ItemKind,
    own: Option<i64>,
    keys: &mut Keys,
) -> Result<(), DbError> {
    let mut kept = Vec::with_capacity(keys.0.len());
    for key in keys.0.drain(..) {
        let column = match key.scheme {
            "tmdb_movie" | "tmdb_tv" => "CAST(tmdb_id AS TEXT)",
            "tvdb" => "CAST(tvdb_id AS TEXT)",
            "imdb" => "imdb_id",
            _ => "NULL",
        };
        let holder: Option<(i64,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT catalog_item_id FROM catalog_key
             WHERE scheme = ?1 AND value = ?2 AND catalog_item_id IS NOT ?3
             UNION ALL
             SELECT id FROM catalog_item
             WHERE kind = ?4 AND {column} = ?2 AND id IS NOT ?3
             LIMIT 1"
        )))
        .bind(key.scheme)
        .bind(&key.value)
        .bind(own)
        .bind(kind.as_str())
        .fetch_optional(&mut *conn)
        .await?;
        if let Some((holder,)) = holder {
            // A `StashDB` UUID names private content: never logged (spec
            // 0007, server logs).
            let value = if key.scheme.starts_with("stashdb_") {
                "[private]"
            } else {
                key.value.as_str()
            };
            tracing::warn!(
                scheme = key.scheme,
                value,
                holder,
                "natural key already belongs to another catalog item"
            );
        } else {
            kept.push(key);
        }
    }
    keys.0 = kept;
    Ok(())
}

/// Store `keys` for item `id`, replacing its earlier key of each scheme.
async fn write_keys(
    conn: &mut sqlx::SqliteConnection,
    id: i64,
    keys: &Keys,
) -> Result<(), DbError> {
    for key in &keys.0 {
        sqlx::query(
            "INSERT INTO catalog_key (catalog_item_id, scheme, value) VALUES (?, ?, ?)
             ON CONFLICT (catalog_item_id, scheme) DO UPDATE SET value = excluded.value",
        )
        .bind(id)
        .bind(key.scheme)
        .bind(&key.value)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

fn capped(p: &Provenance<'_>) -> DateTime<Utc> {
    p.expires_at
        .min(p.fetched_at + METADATA_MAX_AGE)
        .max(p.fetched_at)
}

fn title_metadata(details: &CatalogDetails) -> String {
    let e = &details.entry;
    json!({
        "originalTitle": e.original_title,
        "originalLanguage": e.original_language,
        "overview": e.overview,
        "releaseDate": e.release_date.map(|d| d.to_string()),
        "backdrop": e.backdrop,
        "genres": details.genres.iter().map(|g| json!({ "id": g.id, "name": g.name })).collect::<Vec<_>>(),
        "rating": e.rating,
        "voteCount": e.vote_count,
        "trailer": details.trailer.as_ref().map(|t| json!({ "site": t.site, "key": t.key })),
    })
    .to_string()
}

fn album_metadata(entry: &CatalogEntry) -> String {
    let album = entry.album.as_ref();
    json!({
        "releaseDate": entry.release_date.map(|d| d.to_string()),
        "artistCredit": album.map(|a| a.artist_credit.as_str()),
        "primaryType": album.and_then(|a| a.primary_type),
        "secondaryTypes": album.map(|a| a.secondary_types.as_slice()),
        "trackCount": album.and_then(|a| a.track_count),
    })
    .to_string()
}

fn season_metadata(season: &Season) -> String {
    json!({
        "episodeCount": season.episode_count,
        "airDate": season.air_date.map(|d| d.to_string()),
        "isSpecial": season.is_special,
    })
    .to_string()
}

/// Insert or update the seasons of series `series_id`, matched by number.
async fn upsert_seasons(
    tx: &mut sqlx::SqliteConnection,
    series_id: i64,
    seasons: impl Iterator<Item = &Season>,
    provenance: &Provenance<'_>,
    expires_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), DbError> {
    for season in seasons {
        sqlx::query(
            "INSERT INTO catalog_item (kind, parent_id, season_number, title, year,
                 poster_path, metadata, metadata_source, metadata_instance_id,
                 metadata_fetched_at, metadata_expires_at, created_at, updated_at)
             VALUES ('season', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
             ON CONFLICT (parent_id, season_number) WHERE kind = 'season' DO UPDATE SET
                 title = excluded.title, year = excluded.year,
                 poster_path = excluded.poster_path, metadata = excluded.metadata,
                 metadata_source = excluded.metadata_source,
                 metadata_instance_id = excluded.metadata_instance_id,
                 metadata_fetched_at = excluded.metadata_fetched_at,
                 metadata_expires_at = excluded.metadata_expires_at,
                 updated_at = excluded.updated_at",
        )
        .bind(series_id)
        .bind(i64::from(season.number))
        .bind(&season.name)
        .bind(season.air_date.map(|d| d.year()))
        .bind(season.poster.as_deref())
        .bind(season_metadata(season))
        .bind(provenance.adapter_type)
        .bind(provenance.instance_id)
        .bind(ts(provenance.fetched_at))
        .bind(ts(expires_at))
        .bind(ts(now))
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}

impl Database {
    /// Insert or update the item described by catalog `details` (and, for a
    /// series, its seasons), matched by natural key. Idempotent: the same
    /// details twice give the same item and no duplicates. Seasons the
    /// catalog no longer lists are kept (requests may point at them).
    pub async fn upsert_catalog_item(
        &self,
        details: &CatalogDetails,
        provenance: &Provenance<'_>,
        now: DateTime<Utc>,
    ) -> Result<CatalogItem, DbError> {
        let entry = &details.entry;
        let kind = ItemKind::from(entry.item.kind);
        if kind == ItemKind::Album {
            // Albums are children of their artist (`parent_id` CHECK); they
            // are stored with the music catalog (spec 0006 slice 2).
            return Err(DbError::InvalidData(
                "an album is stored under its artist, not on its own".to_owned(),
            ));
        }
        let mut ids = details.external_ids.clone();
        if let Ok(own) = entry.item.id.parse::<u64>()
            && provenance.adapter_type == "tmdb"
        {
            ids.insert(0, ExternalId::Tmdb(own));
        }
        let mut keys = Keys::from_ids(kind, &ids);
        let year = entry.release_date.map(|d| d.year());
        let expires_at = capped(provenance);

        let mut tx = self.begin_write().await?;
        let existing = find_by_keys(&mut tx, kind, &keys).await?;
        drop_taken_keys(&mut tx, kind, existing, &mut keys).await?;
        let id: i64 = if let Some(id) = existing {
            sqlx::query(
                "UPDATE catalog_item SET
                     tmdb_id = coalesce(?1, tmdb_id), tvdb_id = coalesce(?2, tvdb_id),
                     imdb_id = coalesce(?3, imdb_id), title = ?4, year = ?5, poster_path = ?6,
                     metadata = ?7, metadata_source = ?8, metadata_instance_id = ?9,
                     metadata_fetched_at = ?10, metadata_expires_at = ?11, updated_at = ?12
                 WHERE id = ?13",
            )
            .bind(keys.tmdb_column())
            .bind(keys.tvdb_column())
            .bind(keys.imdb_column())
            .bind(&entry.title)
            .bind(year)
            .bind(entry.poster.as_deref())
            .bind(title_metadata(details))
            .bind(provenance.adapter_type)
            .bind(provenance.instance_id)
            .bind(ts(provenance.fetched_at))
            .bind(ts(expires_at))
            .bind(ts(now))
            .bind(id)
            .execute(&mut *tx)
            .await?;
            id
        } else {
            let (id,): (i64,) = sqlx::query_as(
                "INSERT INTO catalog_item (kind, tmdb_id, tvdb_id, imdb_id, title, year,
                     poster_path, metadata, metadata_source, metadata_instance_id,
                     metadata_fetched_at, metadata_expires_at, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
            )
            .bind(kind.as_str())
            .bind(keys.tmdb_column())
            .bind(keys.tvdb_column())
            .bind(keys.imdb_column())
            .bind(&entry.title)
            .bind(year)
            .bind(entry.poster.as_deref())
            .bind(title_metadata(details))
            .bind(provenance.adapter_type)
            .bind(provenance.instance_id)
            .bind(ts(provenance.fetched_at))
            .bind(ts(expires_at))
            .bind(ts(now))
            .bind(ts(now))
            .fetch_one(&mut *tx)
            .await?;
            id
        };

        write_keys(&mut tx, id, &keys).await?;
        if kind == ItemKind::Series {
            upsert_seasons(&mut tx, id, details.seasons(), provenance, expires_at, now).await?;
        }
        tx.commit().await?;
        self.get_catalog_item(id)
            .await?
            .ok_or_else(|| DbError::InvalidData("catalog item vanished after upsert".to_owned()))
    }

    /// Insert or update albums `entries` (release groups from the music
    /// catalog) under artist `artist_id` (spec 0006 slice 4), matched by
    /// release-group MBID. An album already stored keeps the artist it was
    /// first stored under (an album credited to two artists is one item).
    /// Refused with [`DbError::InvalidData`] for an entry that is no album
    /// or has no release-group MBID. Returns the items in `entries` order.
    pub async fn upsert_albums(
        &self,
        artist_id: i64,
        entries: &[&CatalogEntry],
        provenance: &Provenance<'_>,
        now: DateTime<Utc>,
    ) -> Result<Vec<CatalogItem>, DbError> {
        let expires_at = capped(provenance);
        let mut tx = self.begin_write().await?;
        let mut ids = Vec::with_capacity(entries.len());
        for entry in entries {
            let release_group = (entry.item.kind == MediaKind::Album)
                .then(|| uuid::Uuid::parse_str(&entry.item.id).ok())
                .flatten()
                .ok_or_else(|| {
                    DbError::InvalidData(format!("{} is no release group", entry.item.id))
                })?;
            let keys = Keys::from_ids(
                ItemKind::Album,
                &[ExternalId::MbReleaseGroup(release_group)],
            );
            let year = entry.release_date.map(|d| d.year());
            let metadata = album_metadata(entry);
            let id = if let Some(id) = find_by_keys(&mut tx, ItemKind::Album, &keys).await? {
                sqlx::query(
                    "UPDATE catalog_item SET title = ?1, year = ?2, poster_path = ?3,
                         metadata = ?4, metadata_source = ?5, metadata_instance_id = ?6,
                         metadata_fetched_at = ?7, metadata_expires_at = ?8, updated_at = ?9
                     WHERE id = ?10",
                )
                .bind(&entry.title)
                .bind(year)
                .bind(entry.poster.as_deref())
                .bind(metadata)
                .bind(provenance.adapter_type)
                .bind(provenance.instance_id)
                .bind(ts(provenance.fetched_at))
                .bind(ts(expires_at))
                .bind(ts(now))
                .bind(id)
                .execute(&mut *tx)
                .await?;
                id
            } else {
                let (id,): (i64,) = sqlx::query_as(
                    "INSERT INTO catalog_item (kind, parent_id, title, year, poster_path,
                         metadata, metadata_source, metadata_instance_id, metadata_fetched_at,
                         metadata_expires_at, created_at, updated_at)
                     VALUES ('album', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
                     RETURNING id",
                )
                .bind(artist_id)
                .bind(&entry.title)
                .bind(year)
                .bind(entry.poster.as_deref())
                .bind(metadata)
                .bind(provenance.adapter_type)
                .bind(provenance.instance_id)
                .bind(ts(provenance.fetched_at))
                .bind(ts(expires_at))
                .bind(ts(now))
                .fetch_one(&mut *tx)
                .await?;
                write_keys(&mut tx, id, &keys).await?;
                id
            };
            ids.push(id);
        }
        tx.commit().await?;
        let mut items = Vec::with_capacity(ids.len());
        for id in ids {
            items.push(self.get_catalog_item(id).await?.ok_or_else(|| {
                DbError::InvalidData("catalog item vanished after upsert".to_owned())
            })?);
        }
        Ok(items)
    }

    /// Copy key columns that have no `catalog_key` row yet into
    /// `catalog_key`: items an older release created or changed after a
    /// rollback (it writes only the columns) get their keys back when this
    /// release starts again (ADR 0012). A key another item holds stays with
    /// that item. Idempotent; runs at startup until the contract release.
    pub async fn copy_column_keys(&self) -> Result<u64, DbError> {
        let res = sqlx::raw_sql(
            // A column value the older release changed replaces the key
            // (unless another item holds the new value), then missing
            // keys are added.
            "UPDATE OR IGNORE catalog_key SET value = (SELECT CAST(c.tmdb_id AS TEXT)
                 FROM catalog_item c WHERE c.id = catalog_key.catalog_item_id)
             WHERE scheme IN ('tmdb_movie', 'tmdb_tv') AND EXISTS (SELECT 1 FROM catalog_item c
                 WHERE c.id = catalog_key.catalog_item_id AND c.tmdb_id IS NOT NULL
                   AND CAST(c.tmdb_id AS TEXT) <> catalog_key.value);
             UPDATE OR IGNORE catalog_key SET value = (SELECT CAST(c.tvdb_id AS TEXT)
                 FROM catalog_item c WHERE c.id = catalog_key.catalog_item_id)
             WHERE scheme = 'tvdb' AND EXISTS (SELECT 1 FROM catalog_item c
                 WHERE c.id = catalog_key.catalog_item_id AND c.tvdb_id IS NOT NULL
                   AND CAST(c.tvdb_id AS TEXT) <> catalog_key.value);
             UPDATE OR IGNORE catalog_key SET value = (SELECT c.imdb_id
                 FROM catalog_item c WHERE c.id = catalog_key.catalog_item_id)
             WHERE scheme = 'imdb' AND EXISTS (SELECT 1 FROM catalog_item c
                 WHERE c.id = catalog_key.catalog_item_id AND c.imdb_id IS NOT NULL
                   AND c.imdb_id <> catalog_key.value);
             INSERT OR IGNORE INTO catalog_key (catalog_item_id, scheme, value)
                 SELECT id, iif(kind = 'movie', 'tmdb_movie', 'tmdb_tv'), CAST(tmdb_id AS TEXT)
                 FROM catalog_item WHERE tmdb_id IS NOT NULL AND kind IN ('movie', 'series')
                 ORDER BY id;
             INSERT OR IGNORE INTO catalog_key (catalog_item_id, scheme, value)
                 SELECT id, 'tvdb', CAST(tvdb_id AS TEXT)
                 FROM catalog_item WHERE tvdb_id IS NOT NULL AND kind = 'series'
                 ORDER BY id;
             INSERT OR IGNORE INTO catalog_key (catalog_item_id, scheme, value)
                 SELECT id, 'imdb', imdb_id
                 FROM catalog_item WHERE imdb_id IS NOT NULL AND kind IN ('movie', 'series')
                 ORDER BY id;",
        )
        .execute(self.pool())
        .await?;
        let copied = res.rows_affected();
        if copied > 0 {
            tracing::info!(copied, "natural keys updated from the old key columns");
        }
        Ok(copied)
    }

    pub async fn get_catalog_item(&self, id: i64) -> Result<Option<CatalogItem>, DbError> {
        let row: Option<ItemRow> = sqlx::query_as(concat!(
            "SELECT ",
            item_columns!(),
            " FROM catalog_item WHERE id = ?"
        ))
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    /// The seasons of series `series_id`, by number.
    pub async fn catalog_item_seasons(&self, series_id: i64) -> Result<Vec<CatalogItem>, DbError> {
        let rows: Vec<ItemRow> = sqlx::query_as(concat!(
            "SELECT ",
            item_columns!(),
            " FROM catalog_item WHERE parent_id = ? AND kind = 'season' ORDER BY season_number"
        ))
        .bind(series_id)
        .fetch_all(self.pool())
        .await?;
        crate::readable(rows.into_iter().map(TryInto::try_into))
    }

    /// The item of `kind` with any of the natural keys `ids`.
    pub async fn resolve_catalog_item(
        &self,
        kind: MediaKind,
        ids: &[ExternalId],
    ) -> Result<Option<CatalogItem>, DbError> {
        let kind = ItemKind::from(kind);
        let keys = Keys::from_ids(kind, ids);
        let found = {
            let mut conn = self.pool().acquire().await?;
            find_by_keys(&mut conn, kind, &keys).await?
        };
        match found {
            Some(id) => self.get_catalog_item(id).await,
            None => Ok(None),
        }
    }

    /// Record (or re-confirm) that service instance `instance_id` calls
    /// item `item_id` by `remote_id`.
    pub async fn confirm_external_ref(
        &self,
        item_id: i64,
        instance_id: i64,
        remote_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO external_ref (catalog_item_id, service_instance_id, remote_id, confirmed_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT (catalog_item_id, service_instance_id) DO UPDATE SET
                 remote_id = excluded.remote_id, confirmed_at = excluded.confirmed_at",
        )
        .bind(item_id)
        .bind(instance_id)
        .bind(remote_id)
        .bind(ts(now))
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn external_ref(
        &self,
        item_id: i64,
        instance_id: i64,
    ) -> Result<Option<ExternalRef>, DbError> {
        let row: Option<(String, DateTime<Utc>)> = sqlx::query_as(
            "SELECT remote_id, confirmed_at FROM external_ref
             WHERE catalog_item_id = ? AND service_instance_id = ?",
        )
        .bind(item_id)
        .bind(instance_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|(remote_id, confirmed_at)| ExternalRef {
            catalog_item_id: item_id,
            service_instance_id: instance_id,
            remote_id,
            confirmed_at,
        }))
    }

    /// Forget a remote id that turned out stale (the service no longer
    /// knows it). The item stays; its natural keys find it again.
    pub async fn forget_external_ref(
        &self,
        item_id: i64,
        instance_id: i64,
    ) -> Result<bool, DbError> {
        let res = sqlx::query(
            "DELETE FROM external_ref WHERE catalog_item_id = ? AND service_instance_id = ?",
        )
        .bind(item_id)
        .bind(instance_id)
        .execute(self.pool())
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// The item a service reports as `remote_id`: by the cached remote id
    /// first, else by its natural keys (lesson 7: remote ids are a cache,
    /// the natural key is the fallback). A natural-key hit does not record
    /// the remote id; the caller confirms it once the service agrees.
    pub async fn find_item_for_service(
        &self,
        instance_id: i64,
        remote_id: &str,
        kind: MediaKind,
        ids: &[ExternalId],
    ) -> Result<Option<CatalogItem>, DbError> {
        let cached: Option<(i64,)> = sqlx::query_as(
            "SELECT r.catalog_item_id FROM external_ref r
             JOIN catalog_item i ON i.id = r.catalog_item_id
             WHERE r.service_instance_id = ? AND r.remote_id = ? AND i.kind = ?
             ORDER BY r.confirmed_at DESC LIMIT 1",
        )
        .bind(instance_id)
        .bind(remote_id)
        .bind(ItemKind::from(kind).as_str())
        .fetch_optional(self.pool())
        .await?;
        if let Some((id,)) = cached {
            return self.get_catalog_item(id).await;
        }
        self.resolve_catalog_item(kind, ids).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{NewServiceInstance, StoredCredential};
    use crate::test_support::t0;
    use chrono::NaiveDate;
    use starr_core::adapters::SecretClass;
    use starr_core::catalog::{CatalogEntry, CatalogRef, Child, Genre};

    async fn instance(db: &Database, adapter_type: &str, name: &str) -> i64 {
        db.create_service(
            &NewServiceInstance {
                adapter_type,
                name,
                base_url: "http://x",
                public_url: None,
                credential: &StoredCredential {
                    ciphertext: b"c".to_vec(),
                    recipient: "age1x".to_owned(),
                    class: SecretClass::CatalogReadToken,
                },
            },
            t0(),
        )
        .await
        .unwrap()
        .id
    }

    fn series(tmdb: &str, title: &str, ids: Vec<ExternalId>, seasons: &[u32]) -> CatalogDetails {
        CatalogDetails {
            entry: CatalogEntry {
                item: CatalogRef {
                    kind: MediaKind::Series,
                    id: tmdb.into(),
                },
                title: title.into(),
                original_title: title.into(),
                original_language: "en".into(),
                overview: "…".into(),
                release_date: NaiveDate::from_ymd_opt(2008, 1, 20),
                poster: Some("/p.jpg".into()),
                backdrop: None,
                genre_ids: vec![18],
                rating: Some(8.9),
                vote_count: 15000,
                album: None,
                scene: None,
            },
            external_ids: ids,
            genres: vec![Genre {
                kind: MediaKind::Series,
                id: 18,
                name: "Drama".into(),
            }],
            trailer: None,
            certifications: vec![],
            watch_offers: vec![],
            children: seasons
                .iter()
                .map(|n| {
                    Child::Season(Season {
                        number: *n,
                        name: format!("Season {n}"),
                        episode_count: 7,
                        air_date: NaiveDate::from_ymd_opt(2008 + i32::try_from(*n).unwrap(), 1, 1),
                        is_special: *n == 0,
                        poster: None,
                        episodes: Vec::new(),
                    })
                })
                .collect(),
            facts: starr_core::catalog::TitleFacts::default(),
        }
    }

    fn uuid_of(raw: &str) -> uuid::Uuid {
        raw.parse().unwrap()
    }

    fn provenance(instance_id: i64) -> Provenance<'static> {
        Provenance {
            adapter_type: "tmdb",
            instance_id,
            fetched_at: t0(),
            expires_at: t0() + TimeDelta::days(1),
        }
    }

    async fn count(db: &Database) -> i64 {
        let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM catalog_item")
            .fetch_one(db.pool())
            .await
            .unwrap();
        n
    }

    #[tokio::test]
    async fn upserting_the_same_details_twice_is_idempotent() {
        let db = Database::open_in_memory().await.unwrap();
        let tmdb = instance(&db, "tmdb", "TMDB").await;
        let details = series(
            "1396",
            "Breaking Bad",
            vec![
                ExternalId::Tvdb(81189),
                ExternalId::Imdb("tt0903747".into()),
            ],
            &[0, 1, 2],
        );
        let first = db
            .upsert_catalog_item(&details, &provenance(tmdb), t0())
            .await
            .unwrap();
        assert_eq!(first.kind, ItemKind::Series);
        assert_eq!(first.tmdb_id, Some(1396));
        assert_eq!(first.tvdb_id, Some(81189));
        assert_eq!(first.imdb_id.as_deref(), Some("tt0903747"));
        assert_eq!(first.year, Some(2008));
        assert_eq!(first.metadata_source.as_deref(), Some("tmdb"));
        assert_eq!(first.metadata_instance_id, Some(tmdb));
        let meta: serde_json::Value =
            serde_json::from_str(first.metadata.as_deref().unwrap()).unwrap();
        assert_eq!(meta["genres"][0]["name"], "Drama");
        assert_eq!(count(&db).await, 4);

        let mut renamed = details.clone();
        renamed.entry.title = "Breaking Bad (2008)".into();
        renamed.children.truncate(2);
        let later = t0() + TimeDelta::hours(1);
        let again = db
            .upsert_catalog_item(&renamed, &provenance(tmdb), later)
            .await
            .unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(again.title, "Breaking Bad (2008)");
        assert_eq!(again.created_at, first.created_at);
        assert_eq!(again.updated_at, later);
        // Seasons are matched by number and never dropped.
        assert_eq!(count(&db).await, 4);
        let seasons = db.catalog_item_seasons(first.id).await.unwrap();
        assert_eq!(
            seasons.iter().map(|s| s.season_number).collect::<Vec<_>>(),
            vec![Some(0), Some(1), Some(2)]
        );
        assert_eq!(seasons[1].parent_id, Some(first.id));
    }

    #[tokio::test]
    async fn an_album_without_its_artist_is_refused_before_sql() {
        let db = Database::open_in_memory().await.unwrap();
        let tmdb = instance(&db, "tmdb", "TMDB").await;
        let mut album = series("1", "Album", vec![], &[]);
        album.entry.item.kind = MediaKind::Album;
        assert!(matches!(
            db.upsert_catalog_item(&album, &provenance(tmdb), t0())
                .await,
            Err(DbError::InvalidData(_))
        ));
        assert_eq!(count(&db).await, 0);
    }

    fn music(kind: MediaKind, id: &str, title: &str) -> CatalogEntry {
        CatalogEntry {
            item: CatalogRef {
                kind,
                id: id.into(),
            },
            title: title.into(),
            original_title: title.into(),
            original_language: String::new(),
            overview: String::new(),
            release_date: NaiveDate::from_ymd_opt(1997, 5, 21),
            poster: (kind == MediaKind::Album).then(|| format!("caa:{id}")),
            backdrop: None,
            genre_ids: vec![],
            rating: None,
            vote_count: 0,
            album: (kind == MediaKind::Album).then(|| starr_core::catalog::AlbumFacts {
                artist_credit: "Radiohead".into(),
                artist: None,
                primary_type: Some(starr_core::catalog::ReleaseType::Album),
                secondary_types: vec![],
                track_count: Some(12),
                ..starr_core::catalog::AlbumFacts::default()
            }),
            scene: None,
        }
    }

    const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
    const OK_COMPUTER: &str = "b1392450-e666-3926-a536-22c65f834433";
    const KID_A: &str = "1b022e01-4da6-387b-8658-8678046e4cef";

    fn mb_provenance(instance_id: i64) -> Provenance<'static> {
        Provenance {
            adapter_type: "musicbrainz",
            ..provenance(instance_id)
        }
    }

    fn artist_details(id: &str, name: &str) -> CatalogDetails {
        CatalogDetails {
            entry: music(MediaKind::Artist, id, name),
            external_ids: vec![ExternalId::MbArtist(uuid_of(id))],
            ..series("1", "x", vec![], &[])
        }
    }

    #[tokio::test]
    async fn albums_are_stored_under_their_artist_by_release_group() {
        let db = Database::open_in_memory().await.unwrap();
        let mb = instance(&db, "musicbrainz", "MB").await;
        let artist = db
            .upsert_catalog_item(
                &artist_details(RADIOHEAD, "Radiohead"),
                &mb_provenance(mb),
                t0(),
            )
            .await
            .unwrap();
        assert_eq!(artist.kind, ItemKind::Artist);
        let ok = music(MediaKind::Album, OK_COMPUTER, "OK Computer");
        let kid = music(MediaKind::Album, KID_A, "Kid A");
        let stored = db
            .upsert_albums(artist.id, &[&ok, &kid], &mb_provenance(mb), t0())
            .await
            .unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].kind, ItemKind::Album);
        assert_eq!(stored[0].parent_id, Some(artist.id));
        assert_eq!(stored[0].title, "OK Computer");
        assert_eq!(stored[0].year, Some(1997));
        let cover = format!("caa:{OK_COMPUTER}");
        assert_eq!(stored[0].poster_path.as_deref(), Some(cover.as_str()));
        let meta: serde_json::Value =
            serde_json::from_str(stored[0].metadata.as_deref().unwrap()).unwrap();
        assert_eq!(meta["trackCount"], 12);
        let found = db
            .resolve_catalog_item(
                MediaKind::Album,
                &[ExternalId::MbReleaseGroup(uuid_of(OK_COMPUTER))],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.id, stored[0].id);

        // Again (renamed, under another artist): the same rows, the album
        // keeps the artist it was first stored under.
        let other = db
            .upsert_catalog_item(&artist_details(KID_A, "Someone"), &mb_provenance(mb), t0())
            .await
            .unwrap();
        let mut renamed = ok.clone();
        renamed.title = "OK Computer (reissue)".into();
        let again = db
            .upsert_albums(other.id, &[&renamed], &mb_provenance(mb), t0())
            .await
            .unwrap();
        assert_eq!(again[0].id, stored[0].id);
        assert_eq!(again[0].parent_id, Some(artist.id));
        assert_eq!(again[0].title, "OK Computer (reissue)");
        assert_eq!(count(&db).await, 4);

        // Only albums with a release-group MBID are stored.
        let mut bad = ok.clone();
        bad.item.id = "not-an-mbid".into();
        assert!(matches!(
            db.upsert_albums(artist.id, &[&bad], &mb_provenance(mb), t0())
                .await,
            Err(DbError::InvalidData(_))
        ));
    }

    #[tokio::test]
    async fn movie_and_series_ids_are_separate_number_spaces() {
        let db = Database::open_in_memory().await.unwrap();
        let tmdb = instance(&db, "tmdb", "TMDB").await;
        let series_item = db
            .upsert_catalog_item(
                &series("550", "A show", vec![], &[]),
                &provenance(tmdb),
                t0(),
            )
            .await
            .unwrap();
        let mut movie = series("550", "Fight Club", vec![], &[]);
        movie.entry.item.kind = MediaKind::Movie;
        let movie_item = db
            .upsert_catalog_item(&movie, &provenance(tmdb), t0())
            .await
            .unwrap();
        assert_ne!(series_item.id, movie_item.id);
        assert_eq!(movie_item.kind, ItemKind::Movie);
    }

    #[tokio::test]
    async fn items_resolve_by_any_natural_key() {
        let db = Database::open_in_memory().await.unwrap();
        let tmdb = instance(&db, "tmdb", "TMDB").await;
        let item = db
            .upsert_catalog_item(
                &series("1396", "Breaking Bad", vec![ExternalId::Tvdb(81189)], &[]),
                &provenance(tmdb),
                t0(),
            )
            .await
            .unwrap();
        for ids in [
            vec![ExternalId::Tvdb(81189)],
            vec![ExternalId::Tmdb(1396)],
            vec![ExternalId::Imdb("tt0".into()), ExternalId::Tvdb(81189)],
        ] {
            let found = db
                .resolve_catalog_item(MediaKind::Series, &ids)
                .await
                .unwrap();
            assert_eq!(found.map(|i| i.id), Some(item.id), "{ids:?}");
        }
        assert_eq!(
            db.resolve_catalog_item(MediaKind::Movie, &[ExternalId::Tmdb(1396)])
                .await
                .unwrap(),
            None
        );
        // A later catalog answer that only knows the TVDB id still finds it
        // and adds the IMDb id.
        let mut via_tvdb = series("1396", "Breaking Bad", vec![], &[]);
        via_tvdb.external_ids = vec![
            ExternalId::Tvdb(81189),
            ExternalId::Imdb("tt0903747".into()),
        ];
        let same = db
            .upsert_catalog_item(&via_tvdb, &provenance(tmdb), t0())
            .await
            .unwrap();
        assert_eq!(same.id, item.id);
        assert_eq!(same.imdb_id.as_deref(), Some("tt0903747"));
    }

    /// Keys are written to `catalog_key` (and, until the contract
    /// release, the old columns) and read only from `catalog_key`.
    #[tokio::test]
    async fn keys_are_written_to_both_and_read_from_catalog_key_only() {
        let db = Database::open_in_memory().await.unwrap();
        let tmdb = instance(&db, "tmdb", "TMDB").await;
        let item = db
            .upsert_catalog_item(
                &series(
                    "1396",
                    "Breaking Bad",
                    vec![
                        ExternalId::Tvdb(81189),
                        ExternalId::Imdb("tt0903747".into()),
                        // Not a key of a series: ignored.
                        ExternalId::MbArtist(uuid_of("a74b1b7f-71a5-4011-9441-d0b5e4122711")),
                    ],
                    &[1],
                ),
                &provenance(tmdb),
                t0(),
            )
            .await
            .unwrap();
        let keys: Vec<(String, String)> = sqlx::query_as(
            "SELECT scheme, value FROM catalog_key WHERE catalog_item_id = ? ORDER BY scheme",
        )
        .bind(item.id)
        .fetch_all(db.pool())
        .await
        .unwrap();
        let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
        assert_eq!(
            keys,
            vec![
                pair("imdb", "tt0903747"),
                pair("tmdb_tv", "1396"),
                pair("tvdb", "81189")
            ]
        );
        let columns: (Option<i64>, Option<i64>, Option<String>) =
            sqlx::query_as("SELECT tmdb_id, tvdb_id, imdb_id FROM catalog_item WHERE id = ?")
                .bind(item.id)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(columns, (Some(1396), Some(81189), Some("tt0903747".into())));
        // Reads ignore the columns: without the TVDB key, the TVDB id finds
        // nothing even though the column still holds it.
        sqlx::query("DELETE FROM catalog_key WHERE scheme = 'tvdb'")
            .execute(db.pool())
            .await
            .unwrap();
        assert_eq!(
            db.resolve_catalog_item(MediaKind::Series, &[ExternalId::Tvdb(81189)])
                .await
                .unwrap(),
            None
        );
        let read = db.get_catalog_item(item.id).await.unwrap().unwrap();
        assert_eq!(read.tvdb_id, None);
        assert_eq!(read.tmdb_id, Some(1396));
    }

    #[tokio::test]
    async fn a_key_names_one_item_of_any_kind() {
        let db = Database::open_in_memory().await.unwrap();
        let tmdb = instance(&db, "tmdb", "TMDB").await;
        let shared = || vec![ExternalId::Imdb("tt0944947".into())];
        let show = db
            .upsert_catalog_item(
                &series("1399", "GoT", shared(), &[]),
                &provenance(tmdb),
                t0(),
            )
            .await
            .unwrap();
        let mut cut = series("999", "GoT (cut)", shared(), &[]);
        cut.entry.item.kind = MediaKind::Movie;
        let movie = db
            .upsert_catalog_item(&cut, &provenance(tmdb), t0())
            .await
            .unwrap();
        assert_ne!(show.id, movie.id);
        assert_eq!(show.imdb_id.as_deref(), Some("tt0944947"));
        // The movie keeps its TMDB key but not the IMDb id the series holds.
        assert_eq!(movie.tmdb_id, Some(999));
        assert_eq!(movie.imdb_id, None);
    }

    /// An older release on the new schema writes only the key columns: at
    /// the next start the keys are copied, so the item is found again
    /// instead of colliding with the column's unique index.
    #[tokio::test]
    async fn keys_an_older_release_wrote_only_as_columns_are_copied() {
        let db = Database::open_in_memory().await.unwrap();
        let tmdb = instance(&db, "tmdb", "TMDB").await;
        sqlx::query(
            "INSERT INTO catalog_item (kind, tmdb_id, imdb_id, title, created_at, updated_at)
             VALUES ('movie', 603, 'tt0133093', 'The Matrix', '2026-11-01T00:00:00.000Z',
                 '2026-11-01T00:00:00.000Z')",
        )
        .execute(db.pool())
        .await
        .unwrap();
        assert_eq!(db.copy_column_keys().await.unwrap(), 2);
        assert_eq!(db.copy_column_keys().await.unwrap(), 0);
        let mut matrix = series("603", "The Matrix", vec![], &[]);
        matrix.entry.item.kind = MediaKind::Movie;
        let item = db
            .upsert_catalog_item(&matrix, &provenance(tmdb), t0())
            .await
            .unwrap();
        assert_eq!(item.imdb_id.as_deref(), Some("tt0133093"));
        assert_eq!(count(&db).await, 1);
    }

    /// After a rollback the older release may change a key column of an
    /// item that has a key already: the next start moves the key along, and
    /// until then the column still keeps another item from claiming it.
    #[tokio::test]
    async fn key_columns_an_older_release_changed_win_at_the_next_start() {
        let db = Database::open_in_memory().await.unwrap();
        let tmdb = instance(&db, "tmdb", "TMDB").await;
        let show = db
            .upsert_catalog_item(
                &series("1396", "Breaking Bad", vec![ExternalId::Tvdb(81189)], &[]),
                &provenance(tmdb),
                t0(),
            )
            .await
            .unwrap();
        sqlx::query("UPDATE catalog_item SET tvdb_id = 99999 WHERE id = ?")
            .bind(show.id)
            .execute(db.pool())
            .await
            .unwrap();
        // Another series claiming the changed id does not collide with the
        // column's unique index: it simply does not get it.
        let other = db
            .upsert_catalog_item(
                &series("77", "Other", vec![ExternalId::Tvdb(99999)], &[]),
                &provenance(tmdb),
                t0(),
            )
            .await
            .unwrap();
        assert_eq!(other.tvdb_id, None);
        assert_eq!(db.copy_column_keys().await.unwrap(), 1);
        let found = db
            .resolve_catalog_item(MediaKind::Series, &[ExternalId::Tvdb(99999)])
            .await
            .unwrap();
        assert_eq!(found.map(|i| i.id), Some(show.id));
    }

    #[test]
    fn keys_map_to_their_schemes_by_kind() {
        let mbid = uuid_of("a74b1b7f-71a5-4011-9441-d0b5e4122711");
        let scheme = |kind, id: ExternalId| key_of(kind, &id).map(|k| (k.scheme, k.value));
        assert_eq!(
            scheme(ItemKind::Movie, ExternalId::Tmdb(603)),
            Some(("tmdb_movie", "603".into()))
        );
        assert_eq!(scheme(ItemKind::Movie, ExternalId::Tvdb(1)), None);
        assert_eq!(scheme(ItemKind::Series, ExternalId::Tmdb(0)), None);
        assert_eq!(
            scheme(ItemKind::Artist, ExternalId::MbArtist(mbid)),
            Some(("mb_artist", "a74b1b7f-71a5-4011-9441-d0b5e4122711".into()))
        );
        assert_eq!(
            scheme(ItemKind::Album, ExternalId::MbReleaseGroup(mbid)).map(|k| k.0),
            Some("mb_release_group")
        );
        assert_eq!(
            scheme(ItemKind::Scene, ExternalId::StashScene(mbid)).map(|k| k.0),
            Some("stashdb_scene")
        );
        assert_eq!(
            scheme(ItemKind::Performer, ExternalId::StashPerformer(mbid)).map(|k| k.0),
            Some("stashdb_performer")
        );
        assert_eq!(
            scheme(ItemKind::Studio, ExternalId::StashStudio(mbid)).map(|k| k.0),
            Some("stashdb_studio")
        );
        assert_eq!(scheme(ItemKind::Album, ExternalId::MbArtist(mbid)), None);
        for kind in ItemKind::ALL {
            assert_eq!(ItemKind::parse(kind.as_str()).unwrap(), kind);
        }
        assert!(matches!(
            ItemKind::parse("track"),
            Err(DbError::UnknownValue { .. })
        ));
    }

    #[tokio::test]
    async fn a_malformed_imdb_id_is_dropped_not_fatal() {
        let db = Database::open_in_memory().await.unwrap();
        let tmdb = instance(&db, "tmdb", "TMDB").await;
        for bad in ["tt", "tt ", "tt12x", "nm0000001"] {
            let item = db
                .upsert_catalog_item(
                    &series("77", "X", vec![ExternalId::Imdb(bad.into())], &[1]),
                    &provenance(tmdb),
                    t0(),
                )
                .await
                .unwrap();
            assert_eq!(item.imdb_id, None, "{bad}");
        }
    }

    #[tokio::test]
    async fn stale_remote_ids_fall_back_to_the_natural_key() {
        let db = Database::open_in_memory().await.unwrap();
        let tmdb = instance(&db, "tmdb", "TMDB").await;
        let sonarr = instance(&db, "sonarr", "Sonarr").await;
        let item = db
            .upsert_catalog_item(
                &series("1396", "Breaking Bad", vec![ExternalId::Tvdb(81189)], &[]),
                &provenance(tmdb),
                t0(),
            )
            .await
            .unwrap();
        db.confirm_external_ref(item.id, sonarr, "17", t0())
            .await
            .unwrap();
        let natural = [ExternalId::Tvdb(81189)];
        let by_remote = db
            .find_item_for_service(sonarr, "17", MediaKind::Series, &[])
            .await
            .unwrap();
        assert_eq!(by_remote.map(|i| i.id), Some(item.id));

        // Sonarr re-added the series under a new id: the cached id misses,
        // the natural key still finds the item.
        let by_key = db
            .find_item_for_service(sonarr, "42", MediaKind::Series, &natural)
            .await
            .unwrap();
        assert_eq!(by_key.map(|i| i.id), Some(item.id));
        let later = t0() + TimeDelta::minutes(5);
        db.confirm_external_ref(item.id, sonarr, "42", later)
            .await
            .unwrap();
        let reference = db.external_ref(item.id, sonarr).await.unwrap().unwrap();
        assert_eq!(reference.remote_id, "42");
        assert_eq!(reference.confirmed_at, later);
        assert!(db.forget_external_ref(item.id, sonarr).await.unwrap());
        assert_eq!(db.external_ref(item.id, sonarr).await.unwrap(), None);
        assert!(db.get_catalog_item(item.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn metadata_expiry_is_capped_at_six_months() {
        let db = Database::open_in_memory().await.unwrap();
        let tmdb = instance(&db, "tmdb", "TMDB").await;
        let mut p = provenance(tmdb);
        p.expires_at = t0() + TimeDelta::days(400);
        let item = db
            .upsert_catalog_item(&series("1", "X", vec![], &[]), &p, t0())
            .await
            .unwrap();
        assert_eq!(item.metadata_expires_at, Some(t0() + METADATA_MAX_AGE));
        // The schema refuses a longer expiry written around the repository.
        let raw = sqlx::query("UPDATE catalog_item SET metadata_expires_at = ? WHERE id = ?")
            .bind(ts(t0() + TimeDelta::days(200)))
            .bind(item.id)
            .execute(db.pool())
            .await;
        assert!(raw.is_err());
    }

    /// Two members asking about the same title share one catalog item:
    /// the tables carry no member reference at all (lesson 4: shared).
    #[tokio::test]
    async fn catalog_tables_are_shared_not_personal() {
        let db = Database::open_in_memory().await.unwrap();
        for table in ["catalog_item", "external_ref", "service_role", "category"] {
            let columns: Vec<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info(?)")
                .bind(table)
                .fetch_all(db.pool())
                .await
                .unwrap();
            assert!(!columns.is_empty(), "{table}");
            assert!(
                columns.iter().all(|(c,)| !c.contains("member")),
                "{table} has a member column: {columns:?}"
            );
        }
        let tmdb = instance(&db, "tmdb", "TMDB").await;
        let details = series("1396", "Breaking Bad", vec![], &[1]);
        // "Mia" and "Noah" both open the title: one item.
        let for_mia = db
            .upsert_catalog_item(&details, &provenance(tmdb), t0())
            .await
            .unwrap();
        let for_noah = db
            .upsert_catalog_item(&details, &provenance(tmdb), t0())
            .await
            .unwrap();
        assert_eq!(for_mia, for_noah);
        // Deleting the catalog instance keeps the items (metadata loses its
        // instance, not its content).
        db.delete_service(tmdb, t0()).await.unwrap();
        let kept = db.get_catalog_item(for_mia.id).await.unwrap().unwrap();
        assert_eq!(kept.metadata_instance_id, None);
    }
}
