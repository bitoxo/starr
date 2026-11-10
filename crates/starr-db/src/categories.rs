//! Categories and service roles (domain model §2–3, spec 0003 slice 2):
//! which categories are switched on, which service instance plays which
//! role for a category, and each role's settings. Shared data: nothing here
//! belongs to a member.

use chrono::{DateTime, Utc};
use starr_core::adapters::Capability;
use starr_core::catalog::Relevance;
use starr_core::categories::{RoleKind, SourceSettings, has_request_flow};
use starr_core::libraries::ConsumeSettings;
use starr_core::members::Category;

use crate::requests::SourceTarget;
use crate::{Database, DbError, ts};

/// A category and whether the admin switched it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryState {
    pub category: Category,
    pub enabled: bool,
    pub updated_at: DateTime<Utc>,
    /// The operator statement made when the category was switched on
    /// (adult, spec 0007 Q12); `None` when none was made, it was cleared by
    /// switching off, or the category changed since (an older binary
    /// toggled it after a rollback).
    pub statement: Option<OperatorStatement>,
}

impl CategoryState {
    /// Switched on with a valid operator statement: what the privacy gate
    /// reads as "category enabled" (ADR 0014).
    #[must_use]
    pub const fn enabled_with_statement(&self) -> bool {
        self.enabled && self.statement.is_some()
    }
}

/// Who confirmed the operator statement, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorStatement {
    pub at: DateTime<Utc>,
    /// The admin's username.
    pub by: String,
}

/// One service instance in one role for one category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRole {
    pub id: i64,
    pub category: Category,
    pub kind: RoleKind,
    pub service_instance_id: i64,
    pub service_name: String,
    pub adapter_type: String,
    /// Catalogs only: the catalog Discover and search use.
    pub is_default: bool,
    /// JSON per kind (see the migration); `None` until set.
    pub settings: Option<String>,
    /// Sources only: the metadata profile for new artists (Lidarr), kept
    /// beside `settings` for older releases (see [`SourceSettings`]).
    pub metadata_profile_id: Option<i64>,
    /// Catalogs of the private category only: the weekly cap of a follow's
    /// automatic requests (spec 0007 Q4), kept beside `settings` for older
    /// releases (ADR 0013 amendment 2); `None` until set (the default
    /// applies).
    pub follow_cap_per_week: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ServiceRole {
    /// The relevance of a catalog role; `None` until the admin saved one.
    pub fn relevance(&self) -> Result<Option<Relevance>, DbError> {
        match (self.kind, &self.settings) {
            (RoleKind::Catalog, Some(json)) => Ok(Some(serde_json::from_str(json)?)),
            _ => Ok(None),
        }
    }

    /// The settings of a source role (empty until set).
    pub fn source_settings(&self) -> Result<SourceSettings, DbError> {
        let mut settings: SourceSettings = match (self.kind, &self.settings) {
            (RoleKind::Source, Some(json)) => serde_json::from_str(json)?,
            _ => SourceSettings::default(),
        };
        if self.kind == RoleKind::Source {
            settings.metadata_profile_id = self.metadata_profile_id;
        }
        Ok(settings)
    }

    /// The library mapping of a consume role (empty until set).
    pub fn consume_settings(&self) -> Result<ConsumeSettings, DbError> {
        match (self.kind, &self.settings) {
            (RoleKind::Consume, Some(json)) => Ok(serde_json::from_str(json)?),
            _ => Ok(ConsumeSettings::default()),
        }
    }
}

/// A role to add.
#[derive(Debug, Clone, Copy)]
pub struct NewRole<'a> {
    pub category: Category,
    pub kind: RoleKind,
    pub service_instance_id: i64,
    /// Capabilities the instance's adapter declares (domain model §3:
    /// roles only by capability).
    pub offered: &'a [Capability],
}

#[derive(sqlx::FromRow)]
struct RoleRow {
    id: i64,
    category: String,
    kind: String,
    service_instance_id: i64,
    service_name: String,
    adapter_type: String,
    is_default: bool,
    settings: Option<String>,
    metadata_profile_id: Option<i64>,
    follow_cap_per_week: Option<i64>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

macro_rules! role_select {
    () => {
        "SELECT r.id, r.category, r.kind, r.service_instance_id, s.name AS service_name,
                s.adapter_type, r.is_default, r.settings, r.metadata_profile_id,
                r.follow_cap_per_week, r.created_at, r.updated_at
         FROM service_role r JOIN service_instance s ON s.id = r.service_instance_id"
    };
}

fn parse_category(raw: &str) -> Result<Category, DbError> {
    Category::parse(raw).ok_or_else(|| crate::unknown("category", raw))
}

impl TryFrom<RoleRow> for ServiceRole {
    type Error = DbError;

    fn try_from(row: RoleRow) -> Result<Self, DbError> {
        Ok(Self {
            id: row.id,
            category: parse_category(&row.category)?,
            kind: RoleKind::parse(&row.kind)
                .ok_or_else(|| crate::unknown("role kind", &row.kind))?,
            service_instance_id: row.service_instance_id,
            service_name: row.service_name,
            adapter_type: row.adapter_type,
            is_default: row.is_default,
            settings: row.settings,
            metadata_profile_id: row.metadata_profile_id,
            follow_cap_per_week: row.follow_cap_per_week.and_then(|n| u32::try_from(n).ok()),
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

/// Map constraint violations of `service_role` to their rule.
fn role_conflict(err: sqlx::Error) -> DbError {
    if let sqlx::Error::Database(db) = &err {
        if db.is_unique_violation() {
            let message = db.message();
            if message.contains("service_role.category") && !message.contains("kind") {
                return DbError::SourceTaken;
            }
            return DbError::RoleTaken;
        }
        if db.is_foreign_key_violation() {
            return DbError::ServiceMissing;
        }
    }
    DbError::Sqlx(err)
}

type CategoryRow = (
    String,
    bool,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<String>,
);

/// A stored category; the statement counts only if made after (or with)
/// the last switch (ADR 0014).
fn category_state(
    (name, enabled, updated_at, statement_at, statement_by): CategoryRow,
) -> Result<CategoryState, DbError> {
    let statement = match (statement_at, statement_by) {
        (Some(at), Some(by)) if at >= updated_at => Some(OperatorStatement { at, by }),
        _ => None,
    };
    Ok(CategoryState {
        category: parse_category(&name)?,
        enabled,
        updated_at,
        statement,
    })
}

impl Database {
    /// Every category in [`Category::ALL`] order.
    pub async fn list_categories(&self) -> Result<Vec<CategoryState>, DbError> {
        let rows: Vec<CategoryRow> = sqlx::query_as(
            "SELECT name, enabled, updated_at, statement_at, statement_by FROM category",
        )
        .fetch_all(self.pool())
        .await?;
        let mut states = crate::readable(rows.into_iter().map(category_state))?;
        states.sort_by_key(|s| s.category);
        Ok(states)
    }

    /// One category's state.
    pub async fn category_state(&self, category: Category) -> Result<CategoryState, DbError> {
        let row: CategoryRow = sqlx::query_as(
            "SELECT name, enabled, updated_at, statement_at, statement_by FROM category
             WHERE name = ?",
        )
        .bind(category.as_str())
        .fetch_one(self.pool())
        .await?;
        category_state(row)
    }

    /// Switch a category on or off. Switching off clears the operator
    /// statement; switching on without one leaves no valid statement
    /// ([`Self::enable_with_statement`] records it).
    pub async fn set_category_enabled(
        &self,
        category: Category,
        enabled: bool,
        now: DateTime<Utc>,
    ) -> Result<CategoryState, DbError> {
        let row: CategoryRow = sqlx::query_as(
            "UPDATE category SET enabled = ?1, updated_at = ?2,
                 statement_at = iif(?1, statement_at, NULL),
                 statement_by = iif(?1, statement_by, NULL)
             WHERE name = ?3
             RETURNING name, enabled, updated_at, statement_at, statement_by",
        )
        .bind(enabled)
        .bind(ts(now))
        .bind(category.as_str())
        .fetch_one(self.pool())
        .await?;
        category_state(row)
    }

    /// Switch a category on together with the operator statement of admin
    /// `by` (spec 0007 Q12 A).
    pub async fn enable_with_statement(
        &self,
        category: Category,
        by: &str,
        now: DateTime<Utc>,
    ) -> Result<CategoryState, DbError> {
        let row: CategoryRow = sqlx::query_as(
            "UPDATE category SET enabled = 1, updated_at = ?1, statement_at = ?1,
                 statement_by = ?2
             WHERE name = ?3
             RETURNING name, enabled, updated_at, statement_at, statement_by",
        )
        .bind(ts(now))
        .bind(by)
        .bind(category.as_str())
        .fetch_one(self.pool())
        .await?;
        category_state(row)
    }

    /// Roles of one category, or of all; by category, kind, then age.
    pub async fn list_roles(
        &self,
        category: Option<Category>,
    ) -> Result<Vec<ServiceRole>, DbError> {
        let rows: Vec<RoleRow> = sqlx::query_as(concat!(
            role_select!(),
            " WHERE ?1 IS NULL OR r.category = ?1 ORDER BY r.category, r.kind, r.created_at, r.id"
        ))
        .bind(category.map(Category::as_str))
        .fetch_all(self.pool())
        .await?;
        crate::readable(rows.into_iter().map(TryInto::try_into))
    }

    pub async fn get_role(&self, id: i64) -> Result<Option<ServiceRole>, DbError> {
        let row: Option<RoleRow> = sqlx::query_as(concat!(role_select!(), " WHERE r.id = ?"))
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        row.map(TryInto::try_into).transpose()
    }

    /// The default catalog of `category`, if it has a catalog.
    pub async fn default_catalog(
        &self,
        category: Category,
    ) -> Result<Option<ServiceRole>, DbError> {
        let row: Option<RoleRow> = sqlx::query_as(concat!(
            role_select!(),
            " WHERE r.category = ? AND r.kind = 'catalog' AND r.is_default = 1"
        ))
        .bind(category.as_str())
        .fetch_optional(self.pool())
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    /// The source of `category`, if it has one.
    pub async fn category_source(
        &self,
        category: Category,
    ) -> Result<Option<ServiceRole>, DbError> {
        let row: Option<RoleRow> = sqlx::query_as(concat!(
            role_select!(),
            " WHERE r.category = ? AND r.kind = 'source'"
        ))
        .bind(category.as_str())
        .fetch_optional(self.pool())
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    /// The category's source with complete settings, as a new acquisition
    /// takes them along (D9); `None` while the category has no request flow
    /// (adult until phase 4), no source, quality profile or root folder
    /// (music: also no metadata profile). Every
    /// "requestable" answer and every acquisition goes through here.
    pub async fn source_target(&self, category: Category) -> Result<Option<SourceTarget>, DbError> {
        if !has_request_flow(category) {
            return Ok(None);
        }
        let Some(role) = self.category_source(category).await? else {
            return Ok(None);
        };
        let settings = role.source_settings()?;
        let (Some(quality_profile_id), Some(root_folder_path)) =
            (settings.quality_profile_id, settings.root_folder_path)
        else {
            return Ok(None);
        };
        // Lidarr needs a metadata profile on every new artist (spec 0006 Q6).
        if category == Category::Music && settings.metadata_profile_id.is_none() {
            return Ok(None);
        }
        Ok(Some(SourceTarget {
            category,
            service_instance_id: role.service_instance_id,
            quality_profile_id,
            root_folder_path,
            minimum_availability: match category {
                Category::Movies => Some(settings.minimum_availability.unwrap_or_default()),
                _ => None,
            },
            metadata_profile_id: match category {
                Category::Music => settings.metadata_profile_id,
                _ => None,
            },
        }))
    }

    /// Give a service instance a role. Refused without the capability the
    /// role needs ([`DbError::RoleCapabilityMissing`]), for a second source
    /// ([`DbError::SourceTaken`]; switch with [`Database::replace_source`]),
    /// for a role the instance already has ([`DbError::RoleTaken`]) and for
    /// an unknown instance ([`DbError::ServiceMissing`]). The first catalog
    /// of a category becomes its default.
    pub async fn add_role(
        &self,
        new: &NewRole<'_>,
        now: DateTime<Utc>,
    ) -> Result<ServiceRole, DbError> {
        if !new.kind.allowed_for(new.offered) {
            return Err(DbError::RoleCapabilityMissing);
        }
        let mut tx = self.begin_write().await?;
        let id: (i64,) = sqlx::query_as(
            "INSERT INTO service_role (category, kind, service_instance_id, is_default, created_at, updated_at)
             VALUES (?1, ?2, ?3,
                     ?2 = 'catalog' AND NOT EXISTS (
                         SELECT 1 FROM service_role WHERE category = ?1 AND kind = 'catalog'),
                     ?4, ?4)
             RETURNING id",
        )
        .bind(new.category.as_str())
        .bind(new.kind.as_str())
        .bind(new.service_instance_id)
        .bind(ts(now))
        .fetch_one(&mut *tx)
        .await
        .map_err(role_conflict)?;
        if new.kind == RoleKind::Source {
            restore_source_settings(&mut tx, id.0, new.category, new.service_instance_id).await?;
        }
        tx.commit().await?;
        if new.kind == RoleKind::Source {
            // Kept settings may be older than the instance's last lists.
            self.clear_unlisted_source_settings(new.service_instance_id, now)
                .await?;
        }
        self.get_role(id.0)
            .await?
            .ok_or_else(|| DbError::InvalidData("role vanished after insert".to_owned()))
    }

    /// Replace the source of `category` with `new` (domain model §3:
    /// switching). The old source role goes, its settings are kept for that
    /// instance and `new` gets the ones kept for it (spec 0008 ADM-02); open
    /// requests finish at the old instance.
    pub async fn replace_source(
        &self,
        new: &NewRole<'_>,
        now: DateTime<Utc>,
    ) -> Result<ServiceRole, DbError> {
        if new.kind != RoleKind::Source {
            return Err(DbError::InvalidData(
                "replace_source needs a source role".to_owned(),
            ));
        }
        if !new.kind.allowed_for(new.offered) {
            return Err(DbError::RoleCapabilityMissing);
        }
        let mut tx = self.begin_write().await?;
        keep_source_settings(&mut tx, new.category).await?;
        sqlx::query("DELETE FROM service_role WHERE category = ? AND kind = 'source'")
            .bind(new.category.as_str())
            .execute(&mut *tx)
            .await?;
        let id: (i64,) = sqlx::query_as(
            "INSERT INTO service_role (category, kind, service_instance_id, created_at, updated_at)
             VALUES (?, 'source', ?, ?, ?) RETURNING id",
        )
        .bind(new.category.as_str())
        .bind(new.service_instance_id)
        .bind(ts(now))
        .bind(ts(now))
        .fetch_one(&mut *tx)
        .await
        .map_err(role_conflict)?;
        restore_source_settings(&mut tx, id.0, new.category, new.service_instance_id).await?;
        tx.commit().await?;
        self.clear_unlisted_source_settings(new.service_instance_id, now)
            .await?;
        self.get_role(id.0)
            .await?
            .ok_or_else(|| DbError::InvalidData("role vanished after insert".to_owned()))
    }

    /// Remove a role. If it was the default catalog, the oldest remaining
    /// catalog of the category takes over (domain model §3); a source's
    /// settings are kept for its instance (spec 0008 ADM-02). Returns the
    /// removed role.
    pub async fn remove_role(&self, id: i64) -> Result<Option<ServiceRole>, DbError> {
        let Some(role) = self.get_role(id).await? else {
            return Ok(None);
        };
        let mut tx = self.begin_write().await?;
        if role.kind == RoleKind::Source {
            keep_source_settings(&mut tx, role.category).await?;
        }
        let deleted = sqlx::query("DELETE FROM service_role WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        promote_oldest_catalog(&mut tx, role.category).await?;
        tx.commit().await?;
        Ok((deleted == 1).then_some(role))
    }

    /// Make catalog role `id` the default of its category. `false` if it
    /// does not exist or is not a catalog.
    pub async fn set_default_catalog(&self, id: i64, now: DateTime<Utc>) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let target: Option<(String,)> =
            sqlx::query_as("SELECT category FROM service_role WHERE id = ? AND kind = 'catalog'")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some((category,)) = target else {
            return Ok(false);
        };
        sqlx::query(
            "UPDATE service_role SET is_default = 0, updated_at = ?
             WHERE category = ? AND kind = 'catalog' AND is_default = 1 AND id <> ?",
        )
        .bind(ts(now))
        .bind(&category)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE service_role SET is_default = 1, updated_at = ? WHERE id = ?")
            .bind(ts(now))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Replace the library mapping of consume role `id` in one write
    /// transaction, checking again inside it that no other consume role of
    /// the same instance maps any of these libraries (spec 0005 M4: one
    /// category per library and instance), so two concurrent saves cannot
    /// both take one library ([`DbError::LibraryTaken`]). `None` if there is
    /// no such consume role.
    pub async fn set_consume_libraries(
        &self,
        id: i64,
        settings: &ConsumeSettings,
        now: DateTime<Utc>,
    ) -> Result<Option<ServiceRole>, DbError> {
        let mut tx = self.begin_write().await?;
        let role: Option<(i64,)> = sqlx::query_as(
            "SELECT service_instance_id FROM service_role WHERE id = ? AND kind = 'consume'",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((instance,)) = role else {
            return Ok(None);
        };
        let others: Vec<(Option<String>,)> = sqlx::query_as(
            "SELECT settings FROM service_role
             WHERE service_instance_id = ? AND kind = 'consume' AND id <> ?",
        )
        .bind(instance)
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        for (json,) in others {
            let Some(json) = json else { continue };
            let other: ConsumeSettings = serde_json::from_str(&json)?;
            if other
                .libraries
                .iter()
                .any(|o| settings.libraries.iter().any(|m| m.id == o.id))
            {
                return Err(DbError::LibraryTaken);
            }
        }
        sqlx::query("UPDATE service_role SET settings = ?, updated_at = ? WHERE id = ?")
            .bind(serde_json::to_string(settings)?)
            .bind(ts(now))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.get_role(id).await
    }

    /// Store the settings of source role `id`: the JSON in the shape older
    /// releases read, the metadata profile in its own column.
    pub async fn set_source_settings(
        &self,
        id: i64,
        settings: &SourceSettings,
        now: DateTime<Utc>,
    ) -> Result<Option<ServiceRole>, DbError> {
        let json = serde_json::to_string(settings)?;
        let res = sqlx::query(
            "UPDATE service_role SET settings = ?, metadata_profile_id = ?, updated_at = ?
             WHERE id = ? AND kind = 'source'",
        )
        .bind(json)
        .bind(settings.metadata_profile_id)
        .bind(ts(now))
        .bind(id)
        .execute(self.pool())
        .await?;
        if res.rows_affected() == 0 {
            return Ok(None);
        }
        self.get_role(id).await
    }

    /// Store the settings JSON of role `id`. `None` if it does not exist.
    pub async fn set_role_settings(
        &self,
        id: i64,
        settings: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<ServiceRole>, DbError> {
        let res = sqlx::query("UPDATE service_role SET settings = ?, updated_at = ? WHERE id = ?")
            .bind(settings)
            .bind(ts(now))
            .bind(id)
            .execute(self.pool())
            .await?;
        if res.rows_affected() == 0 {
            return Ok(None);
        }
        self.get_role(id).await
    }

    /// Set the weekly follow cap of catalog role `id` (spec 0007 Q4; the
    /// caller checks the range, the schema refuses anything outside 1 to
    /// 100). `None` if there is no such catalog role.
    pub async fn set_follow_cap(
        &self,
        id: i64,
        per_week: u32,
        now: DateTime<Utc>,
    ) -> Result<Option<ServiceRole>, DbError> {
        let res = sqlx::query(
            "UPDATE service_role SET follow_cap_per_week = ?, updated_at = ?
             WHERE id = ? AND kind = 'catalog'",
        )
        .bind(i64::from(per_week))
        .bind(ts(now))
        .bind(id)
        .execute(self.pool())
        .await?;
        if res.rows_affected() == 0 {
            return Ok(None);
        }
        self.get_role(id).await
    }
}

/// Keep the settings of `category`'s source for its instance before the
/// role goes (spec 0008 ADM-02); a source without settings keeps nothing.
async fn keep_source_settings(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    category: Category,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO kept_source_settings
             (category, service_instance_id, settings, metadata_profile_id)
         SELECT category, service_instance_id, settings, metadata_profile_id
         FROM service_role
         WHERE category = ? AND kind = 'source'
           AND (settings IS NOT NULL OR metadata_profile_id IS NOT NULL)
         ON CONFLICT (category, service_instance_id) DO UPDATE SET
             settings = excluded.settings,
             metadata_profile_id = excluded.metadata_profile_id",
    )
    .bind(category.as_str())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Give the new source role `role_id` the settings kept for its instance in
/// `category`, if any, and forget them there.
async fn restore_source_settings(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    role_id: i64,
    category: Category,
    instance_id: i64,
) -> Result<(), DbError> {
    let kept: Option<(Option<String>, Option<i64>)> = sqlx::query_as(
        "DELETE FROM kept_source_settings WHERE category = ? AND service_instance_id = ?
         RETURNING settings, metadata_profile_id",
    )
    .bind(category.as_str())
    .bind(instance_id)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((settings, metadata_profile_id)) = kept {
        sqlx::query("UPDATE service_role SET settings = ?, metadata_profile_id = ? WHERE id = ?")
            .bind(settings)
            .bind(metadata_profile_id)
            .bind(role_id)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

/// Keep "exactly one default catalog while any catalog exists": after a
/// removal (also by deleting the service instance) the oldest remaining
/// catalog takes over.
async fn promote_oldest_catalog(
    tx: &mut sqlx::SqliteConnection,
    category: Category,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE service_role SET is_default = 1 WHERE id = (
             SELECT id FROM service_role WHERE category = ?1 AND kind = 'catalog'
             ORDER BY created_at, id LIMIT 1)
         AND NOT EXISTS (
             SELECT 1 FROM service_role WHERE category = ?1 AND kind = 'catalog' AND is_default = 1)",
    )
    .bind(category.as_str())
    .execute(tx)
    .await?;
    Ok(())
}

impl Database {
    /// After a service instance was deleted (its roles went with it by
    /// cascade): restore the default catalog of every category.
    pub(crate) async fn repair_default_catalogs(
        tx: &mut sqlx::SqliteConnection,
    ) -> Result<(), DbError> {
        for category in Category::ALL {
            promote_oldest_catalog(tx, category).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{NewServiceInstance, StoredCredential};
    use crate::test_support::t0;
    use starr_core::adapters::SecretClass;

    #[tokio::test]
    async fn operator_statement_holds_until_the_category_is_switched_again() {
        let db = Database::open_in_memory().await.unwrap();
        let adult = db.category_state(Category::Adult).await.unwrap();
        assert!(!adult.enabled && adult.statement.is_none());

        // Switched on without a statement (as before this release).
        let on = db
            .set_category_enabled(Category::Adult, true, t0())
            .await
            .unwrap();
        assert!(on.enabled && !on.enabled_with_statement());

        let t1 = t0() + chrono::TimeDelta::minutes(1);
        let on = db
            .enable_with_statement(Category::Adult, "root", t1)
            .await
            .unwrap();
        assert!(on.enabled_with_statement());
        assert_eq!(
            on.statement,
            Some(OperatorStatement {
                at: t1,
                by: "root".into()
            })
        );
        assert_eq!(db.category_state(Category::Adult).await.unwrap(), on);

        // Off clears it; on again without one has none.
        let t2 = t1 + chrono::TimeDelta::minutes(1);
        db.set_category_enabled(Category::Adult, false, t2)
            .await
            .unwrap();
        let (at,): (Option<String>,) =
            sqlx::query_as("SELECT statement_at FROM category WHERE name = 'adult'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(at, None);
        let again = db
            .set_category_enabled(Category::Adult, true, t2)
            .await
            .unwrap();
        assert!(!again.enabled_with_statement());

        // An older binary toggling the category (it bumps updated_at only)
        // invalidates a statement it does not know about.
        db.enable_with_statement(Category::Adult, "root", t2)
            .await
            .unwrap();
        sqlx::query("UPDATE category SET updated_at = ? WHERE name = 'adult'")
            .bind(ts(t2 + chrono::TimeDelta::seconds(1)))
            .execute(db.pool())
            .await
            .unwrap();
        assert!(
            !db.category_state(Category::Adult)
                .await
                .unwrap()
                .enabled_with_statement()
        );
    }

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
                    class: SecretClass::ServiceCredential,
                },
            },
            t0(),
        )
        .await
        .unwrap()
        .id
    }

    fn role(category: Category, kind: RoleKind, id: i64, offered: &[Capability]) -> NewRole<'_> {
        NewRole {
            category,
            kind,
            service_instance_id: id,
            offered,
        }
    }

    const META: &[Capability] = &[Capability::Metadata];
    const REQ: &[Capability] = &[Capability::Requests];

    #[tokio::test]
    async fn source_settings_are_kept_per_instance_for_re_adding() {
        // Spec 0008 ADM-02: removing or switching a source no longer loses
        // what the admin picked for it.
        let db = Database::open_in_memory().await.unwrap();
        let radarr = instance(&db, "radarr", "Radarr").await;
        let other = instance(&db, "radarr", "Radarr 4K").await;
        let source = db
            .add_role(&role(Category::Movies, RoleKind::Source, radarr, REQ), t0())
            .await
            .unwrap();
        let chosen = SourceSettings {
            quality_profile_id: Some(4),
            root_folder_path: Some("/movies/".into()),
            minimum_availability: Some(starr_core::categories::MinimumAvailability::Released),
            metadata_profile_id: None,
        };
        db.set_source_settings(source.id, &chosen, t0())
            .await
            .unwrap()
            .unwrap();

        // Removed and added again: the settings come back.
        db.remove_role(source.id).await.unwrap().unwrap();
        assert_eq!(db.category_source(Category::Movies).await.unwrap(), None);
        let again = db
            .add_role(&role(Category::Movies, RoleKind::Source, radarr, REQ), t0())
            .await
            .unwrap();
        assert_eq!(again.source_settings().unwrap(), chosen);

        // Switched to another instance (fresh there) and back: kept per
        // instance, so each comes back with its own.
        let switched = db
            .replace_source(&role(Category::Movies, RoleKind::Source, other, REQ), t0())
            .await
            .unwrap();
        assert_eq!(
            switched.source_settings().unwrap(),
            SourceSettings::default()
        );
        let back = db
            .replace_source(&role(Category::Movies, RoleKind::Source, radarr, REQ), t0())
            .await
            .unwrap();
        assert_eq!(back.source_settings().unwrap(), chosen);
        // Another category never gets them.
        let series = db
            .add_role(&role(Category::Series, RoleKind::Source, radarr, REQ), t0())
            .await
            .unwrap();
        assert_eq!(series.source_settings().unwrap(), SourceSettings::default());

        // Kept settings are checked against the instance's lists when they
        // come back: its address may lead to another server by now.
        db.remove_role(back.id).await.unwrap().unwrap();
        let folders = [starr_core::source::RootFolder {
            id: 1,
            path: "/movies/".into(),
            accessible: true,
            free_space_bytes: None,
        }];
        db.store_source_lists(radarr, &[], &folders, &[], t0())
            .await
            .unwrap();
        let back = db
            .add_role(&role(Category::Movies, RoleKind::Source, radarr, REQ), t0())
            .await
            .unwrap();
        let back = db.get_role(back.id).await.unwrap().unwrap();
        assert_eq!(
            back.source_settings().unwrap(),
            SourceSettings {
                quality_profile_id: None,
                ..chosen.clone()
            }
        );

        // Removing the instance forgets them.
        db.remove_role(back.id).await.unwrap().unwrap();
        db.delete_service(radarr, t0()).await.unwrap();
        let (kept,): (i64,) = sqlx::query_as("SELECT count(*) FROM kept_source_settings")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(kept, 0);
    }

    #[tokio::test]
    async fn categories_start_switched_off_and_can_be_switched() {
        let db = Database::open_in_memory().await.unwrap();
        let all = db.list_categories().await.unwrap();
        assert_eq!(
            all.iter().map(|c| c.category).collect::<Vec<_>>(),
            Category::ALL
        );
        assert!(all.iter().all(|c| !c.enabled));
        let on = db
            .set_category_enabled(Category::Movies, true, t0())
            .await
            .unwrap();
        assert!(on.enabled);
        assert!(db.list_categories().await.unwrap()[0].enabled);
    }

    #[tokio::test]
    async fn a_second_source_is_refused_and_switching_replaces_it() {
        let db = Database::open_in_memory().await.unwrap();
        let radarr = instance(&db, "radarr", "Radarr").await;
        let radarr4k = instance(&db, "radarr", "Radarr 4K").await;
        let first = db
            .add_role(&role(Category::Movies, RoleKind::Source, radarr, REQ), t0())
            .await
            .unwrap();
        let err = db
            .add_role(
                &role(Category::Movies, RoleKind::Source, radarr4k, REQ),
                t0(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::SourceTaken), "{err}");
        // The same role twice is its own conflict.
        let err = db
            .add_role(&role(Category::Movies, RoleKind::Source, radarr, REQ), t0())
            .await
            .unwrap_err();
        assert!(
            matches!(err, DbError::SourceTaken | DbError::RoleTaken),
            "{err}"
        );
        // Another category may use the same instance.
        db.add_role(
            &role(Category::Series, RoleKind::Source, radarr4k, REQ),
            t0(),
        )
        .await
        .unwrap();

        let switched = db
            .replace_source(
                &role(Category::Movies, RoleKind::Source, radarr4k, REQ),
                t0(),
            )
            .await
            .unwrap();
        assert_eq!(switched.service_instance_id, radarr4k);
        assert_eq!(db.get_role(first.id).await.unwrap(), None);
        assert_eq!(
            db.category_source(Category::Movies)
                .await
                .unwrap()
                .map(|r| r.id),
            Some(switched.id)
        );
    }

    #[tokio::test]
    async fn roles_need_the_capability() {
        let db = Database::open_in_memory().await.unwrap();
        let tmdb = instance(&db, "tmdb", "TMDB").await;
        let radarr = instance(&db, "radarr", "Radarr").await;
        for (kind, id, offered) in [
            (RoleKind::Source, tmdb, META),
            (RoleKind::Catalog, radarr, REQ),
            (RoleKind::Consume, radarr, REQ),
        ] {
            let err = db
                .add_role(&role(Category::Movies, kind, id, offered), t0())
                .await
                .unwrap_err();
            assert!(
                matches!(err, DbError::RoleCapabilityMissing),
                "{kind:?}: {err}"
            );
        }
        let err = db
            .replace_source(&role(Category::Movies, RoleKind::Source, tmdb, META), t0())
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::RoleCapabilityMissing), "{err}");
        let err = db
            .add_role(&role(Category::Movies, RoleKind::Catalog, 999, META), t0())
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::ServiceMissing), "{err}");
        assert_eq!(db.list_roles(None).await.unwrap(), vec![]);
    }

    #[tokio::test]
    async fn exactly_one_default_catalog_while_any_exists() {
        let db = Database::open_in_memory().await.unwrap();
        let a = instance(&db, "tmdb", "A").await;
        let b = instance(&db, "tmdb", "B").await;
        let c = instance(&db, "tmdb", "C").await;
        let defaults = |db: Database| async move {
            db.list_roles(Some(Category::Movies))
                .await
                .unwrap()
                .into_iter()
                .filter(|r| r.is_default)
                .map(|r| r.service_instance_id)
                .collect::<Vec<_>>()
        };
        let ra = db
            .add_role(&role(Category::Movies, RoleKind::Catalog, a, META), t0())
            .await
            .unwrap();
        assert!(ra.is_default, "the first catalog is the default");
        let later = t0() + chrono::TimeDelta::seconds(1);
        let rb = db
            .add_role(&role(Category::Movies, RoleKind::Catalog, b, META), later)
            .await
            .unwrap();
        let rc = db
            .add_role(
                &role(Category::Movies, RoleKind::Catalog, c, META),
                later + chrono::TimeDelta::seconds(1),
            )
            .await
            .unwrap();
        assert!(!rb.is_default && !rc.is_default);
        assert_eq!(defaults(db.clone()).await, vec![a]);

        assert!(db.set_default_catalog(rc.id, t0()).await.unwrap());
        assert_eq!(defaults(db.clone()).await, vec![c]);
        // The database refuses a second default outright.
        let raw = sqlx::query("UPDATE service_role SET is_default = 1 WHERE id = ?")
            .bind(rb.id)
            .execute(db.pool())
            .await;
        assert!(raw.is_err());

        // Removing the default: the oldest remaining takes over.
        db.remove_role(rc.id).await.unwrap().unwrap();
        assert_eq!(defaults(db.clone()).await, vec![a]);
        // Deleting the default's instance does the same.
        assert!(db.delete_service(a, t0()).await.unwrap());
        assert_eq!(defaults(db.clone()).await, vec![b]);
        assert_eq!(
            db.default_catalog(Category::Movies)
                .await
                .unwrap()
                .map(|r| r.id),
            Some(rb.id)
        );
        // Not a catalog: no default.
        let radarr = instance(&db, "radarr", "Radarr").await;
        let source = db
            .add_role(&role(Category::Movies, RoleKind::Source, radarr, REQ), t0())
            .await
            .unwrap();
        assert!(!db.set_default_catalog(source.id, t0()).await.unwrap());
    }

    #[tokio::test]
    async fn settings_are_stored_and_typed_per_kind() {
        let db = Database::open_in_memory().await.unwrap();
        let tmdb = instance(&db, "tmdb", "TMDB").await;
        let catalog = db
            .add_role(&role(Category::Series, RoleKind::Catalog, tmdb, META), t0())
            .await
            .unwrap();
        assert_eq!(catalog.relevance().unwrap(), None);
        let relevance = Relevance {
            original_languages: vec!["en".into()],
            excluded_genres: vec![10767],
            ..Relevance::default()
        };
        let stored = db
            .set_role_settings(
                catalog.id,
                &serde_json::to_string(&relevance).unwrap(),
                t0(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.relevance().unwrap(), Some(relevance));
        assert_eq!(stored.source_settings().unwrap(), SourceSettings::default());
        assert_eq!(db.set_role_settings(999, "{}", t0()).await.unwrap(), None);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one source's settings through their life, in order
    async fn music_sources_keep_the_metadata_profile_beside_the_settings() {
        let db = Database::open_in_memory().await.unwrap();
        let lidarr = instance(&db, "lidarr", "Lidarr").await;
        let source = db
            .add_role(&role(Category::Music, RoleKind::Source, lidarr, REQ), t0())
            .await
            .unwrap();
        let partial = SourceSettings {
            quality_profile_id: Some(1),
            root_folder_path: Some("/music/".into()),
            minimum_availability: None,
            metadata_profile_id: None,
        };
        db.set_source_settings(source.id, &partial, t0())
            .await
            .unwrap()
            .unwrap();
        // Without a metadata profile Lidarr cannot add an artist (Q6).
        assert_eq!(db.source_target(Category::Music).await.unwrap(), None);

        let complete = SourceSettings {
            metadata_profile_id: Some(3),
            ..partial
        };
        let stored = db
            .set_source_settings(source.id, &complete, t0())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.source_settings().unwrap(), complete);
        assert_eq!(stored.metadata_profile_id, Some(3));
        // The JSON keeps the shape releases before this one read.
        let json = stored.settings.as_deref().unwrap();
        assert!(!json.contains("metadata"), "{json}");
        // Complete: music requests target Lidarr with the profile (spec
        // 0006 slice 4).
        let target = db.source_target(Category::Music).await.unwrap().unwrap();
        assert_eq!(target.service_instance_id, lidarr);
        assert_eq!(target.metadata_profile_id, Some(3));
        assert_eq!(target.minimum_availability, None);

        // A new address of the same instance keeps the settings (spec 0008
        // ADM-01)...
        db.update_service(
            lidarr,
            &crate::services::ServiceUpdate {
                base_url: Some("http://lidarr.lan:8686"),
                ..Default::default()
            },
            t0(),
        )
        .await
        .unwrap();
        let kept = db.get_role(source.id).await.unwrap().unwrap();
        assert_eq!(kept.source_settings().unwrap(), complete);
        // ...and the lists read from there decide: what that server does
        // not offer is cleared (another Lidarr), and the category is named.
        let profiles = [starr_core::source::QualityProfile {
            id: 1,
            name: "Lossless".into(),
        }];
        let folders = [starr_core::source::RootFolder {
            id: 1,
            path: "/music/".into(),
            accessible: true,
            free_space_bytes: None,
        }];
        let same_metadata = [starr_core::source::MetadataProfile {
            id: 3,
            name: "Standard".into(),
            primary_types: vec![],
            secondary_types: vec![],
        }];
        assert_eq!(
            db.store_source_lists(lidarr, &profiles, &folders, &same_metadata, t0())
                .await
                .unwrap(),
            vec![]
        );
        assert_eq!(
            db.get_role(source.id)
                .await
                .unwrap()
                .unwrap()
                .source_settings()
                .unwrap(),
            complete
        );
        assert_eq!(
            db.store_source_lists(lidarr, &profiles, &folders, &[], t0())
                .await
                .unwrap(),
            vec![Category::Music]
        );
        let reset = db.get_role(source.id).await.unwrap().unwrap();
        assert_eq!(
            reset.source_settings().unwrap(),
            SourceSettings {
                metadata_profile_id: None,
                ..complete.clone()
            }
        );
        assert_eq!(db.source_target(Category::Music).await.unwrap(), None);

        // Only sources take source settings.
        let catalog = db
            .add_role(
                &role(
                    Category::Music,
                    RoleKind::Catalog,
                    instance(&db, "musicbrainz", "MB").await,
                    META,
                ),
                t0(),
            )
            .await
            .unwrap();
        assert_eq!(
            db.set_source_settings(catalog.id, &complete, t0())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn the_weekly_follow_cap_lives_beside_the_catalog_settings() {
        let db = Database::open_in_memory().await.unwrap();
        let stashdb = instance(&db, "stashdb", "StashDB").await;
        let catalog = db
            .add_role(
                &role(Category::Adult, RoleKind::Catalog, stashdb, META),
                t0(),
            )
            .await
            .unwrap();
        assert_eq!(catalog.follow_cap_per_week, None);
        let stored = db
            .set_follow_cap(catalog.id, 25, t0())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.follow_cap_per_week, Some(25));
        // The settings JSON keeps the shape older releases read (ADR 0013
        // amendment 2).
        assert_eq!(stored.settings, None);
        // Out of range is refused by the schema too.
        assert!(db.set_follow_cap(catalog.id, 0, t0()).await.is_err());
        assert!(db.set_follow_cap(catalog.id, 101, t0()).await.is_err());
        // Only catalogs take it.
        let whisparr = instance(&db, "whisparr", "Whisparr").await;
        let source = db
            .add_role(
                &role(Category::Adult, RoleKind::Source, whisparr, REQ),
                t0(),
            )
            .await
            .unwrap();
        assert_eq!(db.set_follow_cap(source.id, 5, t0()).await.unwrap(), None);
        assert_eq!(db.set_follow_cap(999, 5, t0()).await.unwrap(), None);
    }
}
