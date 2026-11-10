//! Quality profiles, root folders and (Lidarr) metadata profiles of source
//! instances as last read by
//! the `sources.lists` job (spec 0003 D9). The admin picks a category's
//! profile and folder from these; the API validates against them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use starr_core::categories::SourceSettings;
use starr_core::members::Category;
use starr_core::source::{MetadataProfile, QualityProfile, RootFolder};

use crate::{Database, DbError, ts};

/// The lists of one source instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLists {
    pub quality_profiles: Vec<QualityProfile>,
    pub root_folders: Vec<RootFolder>,
    /// Empty for sources without metadata profiles (Radarr, Sonarr).
    pub metadata_profiles: Vec<MetadataProfile>,
    /// Last successful read; `None` if none succeeded yet.
    pub fetched_at: Option<DateTime<Utc>>,
    /// Error code of the last read, if it failed (the lists of the last
    /// successful read stay).
    pub error: Option<String>,
    pub attempted_at: DateTime<Utc>,
}

/// Profiles JSON, folders JSON, metadata profiles JSON, fetched at, error,
/// attempted at.
type ListsRow = (
    String,
    String,
    Option<String>,
    Option<DateTime<Utc>>,
    Option<String>,
    DateTime<Utc>,
);

#[derive(Serialize, Deserialize)]
struct ProfileJson {
    id: i64,
    name: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataProfileJson {
    id: i64,
    name: String,
    primary_types: Vec<String>,
    secondary_types: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FolderJson {
    id: i64,
    path: String,
    accessible: bool,
    free_space_bytes: Option<u64>,
}

impl Database {
    pub async fn source_lists(&self, instance_id: i64) -> Result<Option<SourceLists>, DbError> {
        let row: Option<ListsRow> = sqlx::query_as(
            "SELECT quality_profiles, root_folders, metadata_profiles, fetched_at, error, attempted_at
             FROM source_lists WHERE service_instance_id = ?",
        )
        .bind(instance_id)
        .fetch_optional(self.pool())
        .await?;
        let Some((profiles, folders, metadata, fetched_at, error, attempted_at)) = row else {
            return Ok(None);
        };
        let profiles: Vec<ProfileJson> = serde_json::from_str(&profiles)?;
        let folders: Vec<FolderJson> = serde_json::from_str(&folders)?;
        let metadata: Vec<MetadataProfileJson> = match metadata {
            Some(json) => serde_json::from_str(&json)?,
            None => Vec::new(),
        };
        Ok(Some(SourceLists {
            quality_profiles: profiles
                .into_iter()
                .map(|p| QualityProfile {
                    id: p.id,
                    name: p.name,
                })
                .collect(),
            root_folders: folders
                .into_iter()
                .map(|f| RootFolder {
                    id: f.id,
                    path: f.path,
                    accessible: f.accessible,
                    free_space_bytes: f.free_space_bytes,
                })
                .collect(),
            metadata_profiles: metadata
                .into_iter()
                .map(|p| MetadataProfile {
                    id: p.id,
                    name: p.name,
                    primary_types: p.primary_types,
                    secondary_types: p.secondary_types,
                })
                .collect(),
            fetched_at,
            error,
            attempted_at,
        }))
    }

    /// Store a successful read. Nothing is stored for an instance that no
    /// longer exists.
    ///
    /// The lists decide whether the instance's source roles keep their
    /// settings (spec 0008 ADM-01): a quality profile, root folder or
    /// metadata profile they no longer offer (the address now leads to
    /// another server) is cleared, in the same transaction. Returns the
    /// categories whose settings lost something; until an admin picks
    /// again, their requests stay closed.
    pub async fn store_source_lists(
        &self,
        instance_id: i64,
        profiles: &[QualityProfile],
        folders: &[RootFolder],
        metadata_profiles: &[MetadataProfile],
        now: DateTime<Utc>,
    ) -> Result<Vec<Category>, DbError> {
        self.store_source_listing(instance_id, profiles, folders, metadata_profiles, true, now)
            .await
    }

    /// [`Self::store_source_lists`] for a read that may be partial: settings
    /// are cleared only when `complete` proves the lists are everything the
    /// source has (ADR 0009, `Listing::complete`).
    pub async fn store_source_listing(
        &self,
        instance_id: i64,
        profiles: &[QualityProfile],
        folders: &[RootFolder],
        metadata_profiles: &[MetadataProfile],
        complete: bool,
        now: DateTime<Utc>,
    ) -> Result<Vec<Category>, DbError> {
        let lists = (profiles, folders, metadata_profiles);
        let profiles = serde_json::to_string(
            &profiles
                .iter()
                .map(|p| ProfileJson {
                    id: p.id,
                    name: p.name.clone(),
                })
                .collect::<Vec<_>>(),
        )?;
        let folders = serde_json::to_string(
            &folders
                .iter()
                .map(|f| FolderJson {
                    id: f.id,
                    path: f.path.clone(),
                    accessible: f.accessible,
                    free_space_bytes: f.free_space_bytes,
                })
                .collect::<Vec<_>>(),
        )?;
        let metadata_profiles = serde_json::to_string(
            &metadata_profiles
                .iter()
                .map(|p| MetadataProfileJson {
                    id: p.id,
                    name: p.name.clone(),
                    primary_types: p.primary_types.clone(),
                    secondary_types: p.secondary_types.clone(),
                })
                .collect::<Vec<_>>(),
        )?;
        let mut tx = self.begin_write().await?;
        sqlx::query(
            "INSERT INTO source_lists (service_instance_id, quality_profiles, root_folders, metadata_profiles, fetched_at, error, attempted_at)
             SELECT ?1, ?2, ?3, ?5, ?4, NULL, ?4 WHERE EXISTS (SELECT 1 FROM service_instance WHERE id = ?1)
             ON CONFLICT (service_instance_id) DO UPDATE SET
                 quality_profiles = excluded.quality_profiles,
                 root_folders = excluded.root_folders,
                 metadata_profiles = excluded.metadata_profiles,
                 fetched_at = excluded.fetched_at,
                 error = NULL,
                 attempted_at = excluded.attempted_at",
        )
        .bind(instance_id)
        .bind(profiles)
        .bind(folders)
        .bind(ts(now))
        .bind(metadata_profiles)
        .execute(&mut *tx)
        .await?;
        let cleared = if complete {
            clear_unlisted(&mut tx, instance_id, lists, now).await?
        } else {
            Vec::new()
        };
        tx.commit().await?;
        Ok(cleared)
    }

    /// Check the source settings of instance `instance_id` against its last
    /// successful lists, as after a read (settings restored for re-adding,
    /// spec 0008 ADM-02). Nothing happens while no lists were read.
    pub async fn clear_unlisted_source_settings(
        &self,
        instance_id: i64,
        now: DateTime<Utc>,
    ) -> Result<Vec<Category>, DbError> {
        let Some(lists) = self.source_lists(instance_id).await? else {
            return Ok(Vec::new());
        };
        if lists.fetched_at.is_none() {
            return Ok(Vec::new());
        }
        let mut tx = self.begin_write().await?;
        let cleared = clear_unlisted(
            &mut tx,
            instance_id,
            (
                &lists.quality_profiles,
                &lists.root_folders,
                &lists.metadata_profiles,
            ),
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(cleared)
    }
    /// Record a failed read with error `code`, keeping earlier lists.
    pub async fn store_source_lists_error(
        &self,
        instance_id: i64,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO source_lists (service_instance_id, error, attempted_at)
             SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM service_instance WHERE id = ?1)
             ON CONFLICT (service_instance_id) DO UPDATE SET
                 error = excluded.error,
                 attempted_at = excluded.attempted_at",
        )
        .bind(instance_id)
        .bind(code)
        .bind(ts(now))
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

/// Clear what `lists` no longer offer from the source roles of instance
/// `instance_id` (spec 0008 ADM-01); returns their categories.
async fn clear_unlisted(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    instance_id: i64,
    lists: (&[QualityProfile], &[RootFolder], &[MetadataProfile]),
    now: DateTime<Utc>,
) -> Result<Vec<Category>, DbError> {
    let roles: Vec<(i64, String, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT id, category, settings, metadata_profile_id FROM service_role
         WHERE service_instance_id = ? AND kind = 'source' ORDER BY category",
    )
    .bind(instance_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut cleared = Vec::new();
    for (id, category, settings, metadata_profile_id) in roles {
        let mut stored: SourceSettings = match settings {
            Some(json) => serde_json::from_str(&json)?,
            None => SourceSettings::default(),
        };
        stored.metadata_profile_id = metadata_profile_id;
        if !stored.retain_listed(lists.0, lists.1, lists.2) {
            continue;
        }
        sqlx::query(
            "UPDATE service_role SET settings = ?, metadata_profile_id = ?, updated_at = ?
         WHERE id = ?",
        )
        .bind(serde_json::to_string(&stored)?)
        .bind(stored.metadata_profile_id)
        .bind(ts(now))
        .bind(id)
        .execute(&mut **tx)
        .await?;
        cleared
            .push(Category::parse(&category).ok_or_else(|| crate::unknown("category", &category))?);
    }
    Ok(cleared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{NewServiceInstance, StoredCredential};
    use crate::test_support::t0;
    use starr_core::adapters::SecretClass;

    #[tokio::test]
    async fn lists_are_stored_and_errors_keep_the_last_good_read() {
        let db = Database::open_in_memory().await.unwrap();
        let id = db
            .create_service(
                &NewServiceInstance {
                    adapter_type: "radarr",
                    name: "Radarr",
                    base_url: "http://r",
                    public_url: None,
                    credential: &StoredCredential {
                        ciphertext: b"c".to_vec(),
                        recipient: "age1x".to_owned(),
                        class: SecretClass::ServiceCredential,
                    },
                },
                t0(),
            )
            .await
            .unwrap()
            .id;
        assert_eq!(db.source_lists(id).await.unwrap(), None);
        db.store_source_lists_error(id, "transient", t0())
            .await
            .unwrap();
        let first = db.source_lists(id).await.unwrap().unwrap();
        assert_eq!(first.fetched_at, None);
        assert_eq!(first.error.as_deref(), Some("transient"));

        let profiles = [QualityProfile {
            id: 4,
            name: "HD-1080p".into(),
        }];
        let folders = [RootFolder {
            id: 1,
            path: "/movies".into(),
            accessible: true,
            free_space_bytes: Some(10),
        }];
        db.store_source_lists(id, &profiles, &folders, &[], t0())
            .await
            .unwrap();
        let later = t0() + chrono::TimeDelta::minutes(1);
        db.store_source_lists_error(id, "credentials_invalid", later)
            .await
            .unwrap();
        let lists = db.source_lists(id).await.unwrap().unwrap();
        assert_eq!(lists.quality_profiles, profiles);
        assert_eq!(lists.root_folders, folders);
        assert_eq!(lists.metadata_profiles, vec![]);
        assert_eq!(lists.fetched_at, Some(t0()));
        assert_eq!(lists.attempted_at, later);
        assert_eq!(lists.error.as_deref(), Some("credentials_invalid"));

        // A new URL may be another server: its lists are gone.
        db.update_service(
            id,
            &crate::services::ServiceUpdate {
                base_url: Some("http://other-radarr"),
                ..Default::default()
            },
            later,
        )
        .await
        .unwrap();
        assert_eq!(db.source_lists(id).await.unwrap(), None);
        db.store_source_lists(id, &profiles, &folders, &[], t0())
            .await
            .unwrap();

        // Unknown instances store nothing; deleting removes the lists.
        db.store_source_lists(999, &profiles, &folders, &[], t0())
            .await
            .unwrap();
        assert_eq!(db.source_lists(999).await.unwrap(), None);
        db.delete_service(id, t0()).await.unwrap();
        assert_eq!(db.source_lists(id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn metadata_profiles_are_stored_with_the_lists() {
        let db = Database::open_in_memory().await.unwrap();
        let id = db
            .create_service(
                &NewServiceInstance {
                    adapter_type: "lidarr",
                    name: "Lidarr",
                    base_url: "http://l",
                    public_url: None,
                    credential: &StoredCredential {
                        ciphertext: b"c".to_vec(),
                        recipient: "age1x".to_owned(),
                        class: SecretClass::ServiceCredential,
                    },
                },
                t0(),
            )
            .await
            .unwrap()
            .id;
        let metadata = [MetadataProfile {
            id: 1,
            name: "Standard".into(),
            primary_types: vec!["Album".into(), "EP".into()],
            secondary_types: vec!["Studio".into()],
        }];
        db.store_source_lists(id, &[], &[], &metadata, t0())
            .await
            .unwrap();
        let lists = db.source_lists(id).await.unwrap().unwrap();
        assert_eq!(lists.metadata_profiles, metadata);
        // A row an older release stored has no metadata profiles.
        sqlx::query("UPDATE source_lists SET metadata_profiles = NULL")
            .execute(db.pool())
            .await
            .unwrap();
        let lists = db.source_lists(id).await.unwrap().unwrap();
        assert_eq!(lists.metadata_profiles, vec![]);
    }
}
