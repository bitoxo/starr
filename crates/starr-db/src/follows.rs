//! Follows (domain model §6, spec 0003 D8, slice 7; spec 0006 slice 6).
//!
//! A follow is personal: every query filters by the owner or returns the
//! owner with each row (lesson 4). At most one follow per member and
//! series or artist is active; unfollowing ends it and keeps the row. A
//! series follow's cutoff (`seasons_through`) only ever moves up, so a
//! season `follows.check` handled once is never new again; an artist
//! follow's cutoff is the follow date (`known_through`) and stays, and an
//! album the member has asked for once is never asked for again by it
//! ([`Database::requested_albums`]).
//!
//! Performer and studio follows (spec 0007 slice 5) start at a date like
//! artists; a scene is asked for once ([`Database::requested_scenes`]).
//! Their requests carry the follow (`request.follow_id`), which is what
//! the weekly cap counts ([`Database::follow_requests_since`]).

use std::collections::BTreeSet;

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqliteConnection;
use starr_core::members::{Category, MemberStatus};
use uuid::Uuid;

use crate::catalog::ItemKind;
use crate::requests::{NewRequest, RequestFollow};
use crate::{Database, DbError, notifications, ts};

/// Where a follow starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cutoff {
    /// Series: regular seasons up to this number were known; later ones
    /// are new. Only ever moves up.
    Seasons(u32),
    /// Artists, performers and studios: release groups or scenes released
    /// after this date are new (the follow date, spec 0006 Q3, spec 0007
    /// Q4). Never moves.
    Date(NaiveDate),
}

impl Cutoff {
    /// The seasons cutoff, if this is one.
    #[must_use]
    pub const fn seasons(self) -> Option<u32> {
        match self {
            Self::Seasons(n) => Some(n),
            Self::Date(_) => None,
        }
    }

    /// The date cutoff, if this is one.
    #[must_use]
    pub const fn date(self) -> Option<NaiveDate> {
        match self {
            Self::Seasons(_) => None,
            Self::Date(d) => Some(d),
        }
    }
}

/// How `known_through` is stored.
const DATE_FORMAT: &str = "%Y-%m-%d";

/// An active follow with its series or artist and owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowRecord {
    pub id: i64,
    pub member_id: i64,
    pub member_username: String,
    pub member_status: MemberStatus,
    /// The series, artist, performer or studio.
    pub catalog_item_id: i64,
    /// [`ItemKind::Series`], [`ItemKind::Artist`], [`ItemKind::Performer`]
    /// or [`ItemKind::Studio`].
    pub kind: ItemKind,
    /// Series.
    pub tmdb_id: Option<u64>,
    /// Artists.
    pub mb_artist: Option<Uuid>,
    /// Performers and studios: the `StashDB` UUID (private: never logged).
    pub stash_id: Option<Uuid>,
    pub title: String,
    pub year: Option<i32>,
    pub poster_path: Option<String>,
    pub category: Category,
    pub cutoff: Cutoff,
    pub created_at: DateTime<Utc>,
    /// The last time `follows.check` looked at it, whatever came of it.
    pub checked_at: Option<DateTime<Utc>>,
    /// Performers and studios: how many new scenes the last check left
    /// beyond the weekly cap (spec 0007 Q4); `None` when none.
    pub capped_scenes: Option<u32>,
}

/// What undoing an unfollow found ([`Database::restore_follow`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restored {
    /// Active again, as it was.
    Back(FollowRecord),
    /// Still active: nothing to undo.
    Active(FollowRecord),
    /// The member follows the same title anew; the old follow stays ended.
    Superseded,
    /// Not the member's, of another category, or ended too long ago.
    NotFound,
}

#[derive(sqlx::FromRow)]
struct FollowRow {
    id: i64,
    member_id: i64,
    username: String,
    status: String,
    catalog_item_id: i64,
    kind: String,
    tmdb_id: Option<i64>,
    mb_artist: Option<String>,
    stash_id: Option<String>,
    title: String,
    year: Option<i64>,
    poster_path: Option<String>,
    category: String,
    seasons_through: Option<i64>,
    known_through: Option<String>,
    created_at: DateTime<Utc>,
    checked_at: Option<DateTime<Utc>>,
    capped_scenes: Option<i64>,
}

impl TryFrom<FollowRow> for FollowRecord {
    type Error = DbError;

    fn try_from(row: FollowRow) -> Result<Self, DbError> {
        let kind = ItemKind::parse(&row.kind)?;
        let cutoff =
            match (kind, row.seasons_through, row.known_through.as_deref()) {
                // A cutoff of 0 would ask for every season: refuse bad data.
                (ItemKind::Series, Some(n), _) => Cutoff::Seasons(
                    u32::try_from(n)
                        .map_err(|_| DbError::InvalidData(format!("invalid cutoff {n}")))?,
                ),
                (ItemKind::Artist | ItemKind::Performer | ItemKind::Studio, _, Some(date)) => {
                    Cutoff::Date(NaiveDate::parse_from_str(date, DATE_FORMAT).map_err(|_| {
                        DbError::InvalidData(format!("invalid follow date `{date}`"))
                    })?)
                }
                (
                    ItemKind::Series | ItemKind::Artist | ItemKind::Performer | ItemKind::Studio,
                    _,
                    _,
                ) => {
                    return Err(DbError::InvalidData(format!(
                        "follow {} of a {} has no cutoff of its kind",
                        row.id,
                        kind.as_str()
                    )));
                }
                // Kinds a newer release may follow: left out of lists (ADR
                // 0012).
                (other, _, _) => return Err(crate::unknown("followed kind", other.as_str())),
            };
        Ok(Self {
            id: row.id,
            member_id: row.member_id,
            member_username: row.username,
            member_status: MemberStatus::parse(&row.status)
                .ok_or_else(|| crate::unknown("member status", &row.status))?,
            catalog_item_id: row.catalog_item_id,
            kind,
            tmdb_id: row.tmdb_id.and_then(|v| u64::try_from(v).ok()),
            mb_artist: row
                .mb_artist
                .as_deref()
                .and_then(|v| Uuid::parse_str(v).ok()),
            stash_id: row
                .stash_id
                .as_deref()
                .and_then(|v| Uuid::parse_str(v).ok()),
            title: row.title,
            year: row.year.and_then(|y| i32::try_from(y).ok()),
            poster_path: row.poster_path,
            category: Category::parse(&row.category)
                .ok_or_else(|| crate::unknown("category", &row.category))?,
            cutoff,
            created_at: row.created_at,
            checked_at: row.checked_at,
            capped_scenes: row.capped_scenes.and_then(|n| u32::try_from(n).ok()),
        })
    }
}

/// Active follows with series or artist and owner; filter parameters ?1
/// (follow id), ?2 (member id) and ?3 (last checked at or before, or
/// never), each ignored when NULL. With ?3 the longest unchecked come
/// first (never checked before all), otherwise the newest.
const FOLLOW_SELECT: &str = concat!(
    "SELECT f.id, f.member_id, m.username, m.status, f.catalog_item_id, c.kind, ",
    tmdb_key!("c.id"),
    " AS tmdb_id, ",
    mb_artist_key!("c.id"),
    " AS mb_artist, ",
    key_value!("c.id", "'stashdb_performer', 'stashdb_studio'"),
    " AS stash_id, c.title, c.year, c.poster_path, f.category, f.seasons_through,
           f.known_through, f.created_at, f.checked_at, f.capped_scenes
    FROM follow f
    JOIN member m ON m.id = f.member_id
    JOIN catalog_item c ON c.id = f.catalog_item_id
    WHERE f.ended_at IS NULL AND (?1 IS NULL OR f.id = ?1) AND (?2 IS NULL OR f.member_id = ?2)
      AND (?3 IS NULL OR f.checked_at IS NULL OR f.checked_at <= ?3)
    ORDER BY iif(?3 IS NULL, '', coalesce(f.checked_at, '')), f.created_at DESC, f.id DESC"
);

/// Start following series or artist `catalog_item_id` from `cutoff`, or
/// keep following it: a seasons cutoff moves up to `cutoff`, a date stays
/// (a later one would drop the albums released in between). Returns the
/// follow's id and whether it is new.
pub(crate) async fn start(
    conn: &mut SqliteConnection,
    member_id: i64,
    catalog_item_id: i64,
    category: Category,
    cutoff: Cutoff,
    now: DateTime<Utc>,
) -> Result<(i64, bool), DbError> {
    let seasons = cutoff.seasons().map(i64::from);
    let existing: Option<(i64,)> = sqlx::query_as(
        "UPDATE follow SET seasons_through = iif(?1 IS NULL, seasons_through,
                                                 max(seasons_through, ?1))
         WHERE member_id = ?2 AND catalog_item_id = ?3 AND ended_at IS NULL
         RETURNING id",
    )
    .bind(seasons)
    .bind(member_id)
    .bind(catalog_item_id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((id,)) = existing {
        return Ok((id, false));
    }
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO follow (member_id, catalog_item_id, category, seasons_through,
             known_through, created_at)
         VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(member_id)
    .bind(catalog_item_id)
    .bind(category.as_str())
    .bind(seasons)
    .bind(cutoff.date().map(|d| d.format(DATE_FORMAT).to_string()))
    .bind(ts(now))
    .fetch_one(&mut *conn)
    .await?;
    Ok((id, true))
}

/// Note a check of the member's active follow `follow_id` and move a
/// seasons cutoff up to `seasons_through` (`None` keeps the cutoff). False
/// if there is no such active follow (ended meanwhile, or another
/// member's).
pub(crate) async fn advance(
    conn: &mut SqliteConnection,
    follow_id: i64,
    member_id: i64,
    seasons_through: Option<u32>,
    now: DateTime<Utc>,
) -> Result<bool, DbError> {
    let res = sqlx::query(
        "UPDATE follow SET seasons_through = iif(?1 IS NULL, seasons_through,
                                                 max(seasons_through, ?1)),
             checked_at = ?2
         WHERE id = ?3 AND member_id = ?4 AND ended_at IS NULL",
    )
    .bind(seasons_through.map(i64::from))
    .bind(ts(now))
    .bind(follow_id)
    .bind(member_id)
    .execute(&mut *conn)
    .await?;
    Ok(res.rows_affected() == 1)
}

/// Apply what new request `new` does to the member's follow (D8), in its
/// transaction.
pub(crate) async fn link(
    conn: &mut SqliteConnection,
    new: &NewRequest<'_>,
    now: DateTime<Utc>,
) -> Result<(), DbError> {
    match new.follow {
        RequestFollow::No => Ok(()),
        RequestFollow::Start { cutoff } => {
            start(
                conn,
                new.member_id,
                new.catalog_item_id,
                new.source.category,
                cutoff,
                now,
            )
            .await?;
            Ok(())
        }
        RequestFollow::Found { follow_id, cutoff } => {
            if advance(conn, follow_id, new.member_id, cutoff.seasons(), now).await? {
                Ok(())
            } else {
                Err(DbError::FollowEnded)
            }
        }
    }
}

impl Database {
    /// Follow series or artist `catalog_item_id` in `category` from
    /// `cutoff` on (D8, spec 0006 Q3), or keep following it (a seasons
    /// cutoff only moves up, a date stays). Returns the follow and whether
    /// it is new.
    pub async fn follow(
        &self,
        member_id: i64,
        catalog_item_id: i64,
        category: Category,
        cutoff: Cutoff,
        now: DateTime<Utc>,
    ) -> Result<(FollowRecord, bool), DbError> {
        let mut tx = self.begin_write().await?;
        let (id, created) =
            start(&mut tx, member_id, catalog_item_id, category, cutoff, now).await?;
        tx.commit().await?;
        let follow = self
            .member_follow(member_id, id)
            .await?
            .ok_or_else(|| DbError::InvalidData("follow vanished after insert".to_owned()))?;
        Ok((follow, created))
    }

    /// The member's active follows, newest first.
    pub async fn member_follows(&self, member_id: i64) -> Result<Vec<FollowRecord>, DbError> {
        self.query_follows(None, Some(member_id), None).await
    }

    /// The member's active follow `id`, if it is theirs.
    pub async fn member_follow(
        &self,
        member_id: i64,
        id: i64,
    ) -> Result<Option<FollowRecord>, DbError> {
        Ok(self
            .query_follows(Some(id), Some(member_id), None)
            .await?
            .into_iter()
            .next())
    }

    /// Every member's active follows last checked at or before
    /// `checked_before` or never, never checked first, then the longest
    /// unchecked; each with its owner (`follows.check` only; never for a
    /// member-facing answer).
    pub async fn due_follows(
        &self,
        checked_before: DateTime<Utc>,
    ) -> Result<Vec<FollowRecord>, DbError> {
        self.query_follows(None, None, Some(checked_before)).await
    }

    async fn query_follows(
        &self,
        id: Option<i64>,
        member_id: Option<i64>,
        checked_before: Option<DateTime<Utc>>,
    ) -> Result<Vec<FollowRecord>, DbError> {
        let rows: Vec<FollowRow> = sqlx::query_as(FOLLOW_SELECT)
            .bind(id)
            .bind(member_id)
            .bind(checked_before.map(ts))
            .fetch_all(self.pool())
            .await?;
        crate::readable(rows.into_iter().map(TryInto::try_into))
    }

    /// End the member's follow `follow_id`. False if the member has no
    /// such active follow.
    pub async fn unfollow(
        &self,
        member_id: i64,
        follow_id: i64,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let res = sqlx::query(
            "UPDATE follow SET ended_at = max(?, created_at)
             WHERE id = ? AND member_id = ? AND ended_at IS NULL",
        )
        .bind(ts(now))
        .bind(follow_id)
        .bind(member_id)
        .execute(self.pool())
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Undo an unfollow (spec 0008 F8): the member's follow `follow_id` of
    /// `category` that ended at or after `ended_since` is active again,
    /// exactly as it was (id, cutoff, start, last check and hint). An
    /// active one is returned as it is; one the member has followed anew
    /// meanwhile stays ended ([`Restored::Superseded`]).
    pub async fn restore_follow(
        &self,
        member_id: i64,
        follow_id: i64,
        category: Category,
        ended_since: DateTime<Utc>,
    ) -> Result<Restored, DbError> {
        let mut tx = self.begin_write().await?;
        let row: Option<(Option<String>, i64)> = sqlx::query_as(
            "SELECT ended_at, catalog_item_id FROM follow
             WHERE id = ? AND member_id = ? AND category = ?",
        )
        .bind(follow_id)
        .bind(member_id)
        .bind(category.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        let Some((ended_at, item)) = row else {
            return Ok(Restored::NotFound);
        };
        let reopened = ended_at.is_some();
        if let Some(ended_at) = ended_at {
            if ended_at < ts(ended_since) {
                return Ok(Restored::NotFound);
            }
            let newer: Option<(i64,)> = sqlx::query_as(
                "SELECT id FROM follow
                 WHERE member_id = ? AND catalog_item_id = ? AND ended_at IS NULL",
            )
            .bind(member_id)
            .bind(item)
            .fetch_optional(&mut *tx)
            .await?;
            if newer.is_some() {
                return Ok(Restored::Superseded);
            }
            sqlx::query("UPDATE follow SET ended_at = NULL WHERE id = ? AND ended_at IS NOT NULL")
                .bind(follow_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(self
            .member_follow(member_id, follow_id)
            .await?
            .map_or(Restored::NotFound, |f| {
                if reopened {
                    Restored::Back(f)
                } else {
                    Restored::Active(f)
                }
            }))
    }

    /// `follows.check` looked at the member's follow `follow_id` without a
    /// request (nothing new, skipped or failed): note the check and move a
    /// seasons cutoff up to `seasons_through` (seasons skipped as available
    /// or already requested; `None` keeps the cutoff). False if the follow
    /// ended meanwhile.
    pub async fn follow_checked(
        &self,
        follow_id: i64,
        member_id: i64,
        seasons_through: Option<u32>,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let advanced = advance(&mut tx, follow_id, member_id, seasons_through, now).await?;
        tx.commit().await?;
        Ok(advanced)
    }

    /// Whether any active follow was last checked at or before
    /// `checked_before`, or never (`follows.check` has work).
    pub async fn has_due_follows(&self, checked_before: DateTime<Utc>) -> Result<bool, DbError> {
        let row: Option<(i64,)> = sqlx::query_as(
            // Only kinds this release checks: a newer release's follows
            // (left out of `due_follows`) would never be noted as checked.
            "SELECT 1 FROM follow f JOIN catalog_item c ON c.id = f.catalog_item_id
             WHERE f.ended_at IS NULL AND (f.checked_at IS NULL OR f.checked_at <= ?)
               AND c.kind IN ('series', 'artist', 'performer', 'studio')
             LIMIT 1",
        )
        .bind(ts(checked_before))
        .fetch_optional(self.pool())
        .await?;
        Ok(row.is_some())
    }

    /// The release groups of every album the member has asked for, in any
    /// request and whatever became of it: an artist follow asks for an
    /// album once, and after a withdrawal or a rejection it stays away (as
    /// a season behind a series follow's cutoff does).
    pub async fn requested_albums(&self, member_id: i64) -> Result<BTreeSet<Uuid>, DbError> {
        let rows: Vec<(Option<String>,)> = sqlx::query_as(concat!(
            "SELECT DISTINCT ",
            mb_release_group_key!("c.id"),
            " FROM request_item i JOIN catalog_item c ON c.id = i.catalog_item_id
             WHERE i.member_id = ? AND c.kind = 'album'"
        ))
        .bind(member_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(id,)| id.as_deref().and_then(|v| Uuid::parse_str(v).ok()))
            .collect())
    }

    /// The `StashDB` UUIDs of every scene the member has asked for, in any
    /// request and whatever became of it: a performer or studio follow
    /// asks for a scene once (as an artist follow does for an album).
    pub async fn requested_scenes(&self, member_id: i64) -> Result<BTreeSet<Uuid>, DbError> {
        let rows: Vec<(Option<String>,)> = sqlx::query_as(concat!(
            "SELECT DISTINCT ",
            stash_scene_key!("c.id"),
            " FROM request_item i JOIN catalog_item c ON c.id = i.catalog_item_id
             WHERE i.member_id = ? AND c.kind = 'scene'"
        ))
        .bind(member_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(id,)| id.as_deref().and_then(|v| Uuid::parse_str(v).ok()))
            .collect())
    }

    /// How many requests follow `follow_id` created after `since`: what
    /// its weekly cap counts (spec 0007 Q4). Requests made by hand carry
    /// no follow and never count.
    pub async fn follow_requests_since(
        &self,
        follow_id: i64,
        since: DateTime<Utc>,
    ) -> Result<u32, DbError> {
        let (n,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM request WHERE follow_id = ? AND created_at > ?")
                .bind(follow_id)
                .bind(ts(since))
                .fetch_one(self.pool())
                .await?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    /// Note what the member's follow `follow` left beyond its weekly cap
    /// in this check (`over`, 0 for nothing): the hint on the follow, and
    /// when something was left, the `follow_capped` notification once in
    /// the follow's `week` ([`starr_core::follows::cap_week`]). False if
    /// the follow ended meanwhile.
    pub async fn note_follow_cap(
        &self,
        follow: &FollowRecord,
        over: u32,
        week: i64,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let res = sqlx::query(
            "UPDATE follow SET capped_scenes = ? WHERE id = ? AND member_id = ? AND ended_at IS NULL",
        )
        .bind((over > 0).then_some(i64::from(over)))
        .bind(follow.id)
        .bind(follow.member_id)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            return Ok(false);
        }
        if over > 0 {
            notifications::follow_capped(&mut tx, follow, week, now).await?;
        }
        tx.commit().await?;
        Ok(true)
    }
}

#[cfg(test)]
#[path = "follows_tests.rs"]
mod tests;
