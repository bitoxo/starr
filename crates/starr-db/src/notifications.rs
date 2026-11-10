//! Notifications (spec 0003 O1, O2): streams, channels, member
//! preferences and channel deliveries.
//!
//! Notifications are written in the transaction of the change they report
//! (a request's state change, an acquisition failing), together with one
//! delivery per channel that should carry them, so nothing is reported
//! that did not happen and nothing that happened goes unreported. A
//! `dedup_key` unique per stream keeps retries and flapping states from
//! notifying twice; callers hold a write transaction (`BEGIN IMMEDIATE`),
//! so the existence check before the insert cannot race.
//!
//! Streams are personal (lesson 4): member reads always filter by the
//! member; the admin stream holds only admin kinds, whose payload is the
//! only place a member's username appears.
//!
//! Channel secrets are sealed to the worker's key; this module only moves
//! bytes. Listings ([`ChannelRecord`]) say whether a secret is set, never
//! what it is; only [`Database::due_deliveries`] (worker) returns it.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::SqliteConnection;
use starr_core::adapters::SecretClass;
use starr_core::members::Category;
use starr_core::notifications::{self as rules, ChannelKind, NotificationKind, Payload};
use starr_core::requests::RequestState;

use crate::services::StoredCredential;
use crate::{Database, DbError, parse_ts, ts};

/// Whose notifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// A member's personal stream.
    Member(i64),
    /// The admin stream, shared by all admins.
    Admin,
}

impl Stream {
    fn member_id(self) -> Option<i64> {
        match self {
            Self::Member(id) => Some(id),
            Self::Admin => None,
        }
    }
}

/// One notification as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationRecord {
    pub id: i64,
    pub member_id: Option<i64>,
    pub kind: NotificationKind,
    pub payload: Payload,
    pub created_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
}

type NotificationRow = (i64, Option<i64>, String, String, String, Option<String>);

fn kind(raw: &str) -> Result<NotificationKind, DbError> {
    NotificationKind::parse(raw).ok_or_else(|| crate::unknown("notification kind", raw))
}

fn channel_kind(raw: &str) -> Result<ChannelKind, DbError> {
    ChannelKind::parse(raw).ok_or_else(|| crate::unknown("channel kind", raw))
}

impl TryFrom<NotificationRow> for NotificationRecord {
    type Error = DbError;

    fn try_from(row: NotificationRow) -> Result<Self, DbError> {
        let (id, member_id, raw_kind, payload, created_at, read_at) = row;
        Ok(Self {
            id,
            member_id,
            kind: kind(&raw_kind)?,
            payload: serde_json::from_str(&payload)?,
            created_at: parse_ts(&created_at)?,
            read_at: read_at.as_deref().map(parse_ts).transpose()?,
        })
    }
}

/// A channel as shown to admins: no secret, only whether one is set.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelRecord {
    pub id: i64,
    pub kind: ChannelKind,
    pub name: String,
    pub enabled: bool,
    /// The non-secret settings (SMTP host, port, …; a webhook's host).
    pub settings: Value,
    pub has_secret: bool,
    /// Admin kinds sent to this channel.
    pub admin_events: Vec<NotificationKind>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_ok: Option<bool>,
    pub last_error_code: Option<String>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

type ChannelRow = (
    i64,
    String,
    String,
    bool,
    String,
    bool,
    String,
    Option<String>,
    Option<bool>,
    Option<String>,
    Option<String>,
    String,
    String,
);

const CHANNEL_COLUMNS: &str = "id, kind, name, enabled, settings, secret_ciphertext IS NOT NULL,
    admin_events, last_attempt_at, last_ok, last_error_code, last_error, created_at, updated_at";

fn admin_events(raw: &str) -> Result<Vec<NotificationKind>, DbError> {
    let names: Vec<String> = serde_json::from_str(raw)?;
    crate::readable(names.iter().map(|n| kind(n)))
}

/// Names of every notification and channel kind this release knows, as a
/// JSON array for `json_each`: rows of kinds a newer release added are left
/// alone (ADR 0012).
fn known_kinds_json() -> Result<(String, String), DbError> {
    let notifications: Vec<&str> = NotificationKind::ALL.iter().map(|k| k.as_str()).collect();
    let channels: Vec<&str> = ChannelKind::ALL.iter().map(|k| k.as_str()).collect();
    Ok((
        serde_json::to_string(&notifications)?,
        serde_json::to_string(&channels)?,
    ))
}

/// The stored admin events a newer release added (unknown here), so an
/// update keeps them instead of dropping what it could not read.
fn unknown_events(stored: &str) -> Result<Vec<String>, DbError> {
    let names: Vec<String> = serde_json::from_str(stored)?;
    Ok(names
        .into_iter()
        .filter(|n| NotificationKind::parse(n).is_none())
        .collect())
}

fn events_json(events: &[NotificationKind]) -> Result<String, DbError> {
    let mut names: Vec<&str> = events
        .iter()
        .filter(|k| k.is_admin())
        .map(|k| k.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    Ok(serde_json::to_string(&names)?)
}

impl TryFrom<ChannelRow> for ChannelRecord {
    type Error = DbError;

    fn try_from(row: ChannelRow) -> Result<Self, DbError> {
        let (
            id,
            raw_kind,
            name,
            enabled,
            settings,
            has_secret,
            events,
            last_attempt_at,
            last_ok,
            last_error_code,
            last_error,
            created_at,
            updated_at,
        ) = row;
        Ok(Self {
            id,
            kind: channel_kind(&raw_kind)?,
            name,
            enabled,
            settings: serde_json::from_str(&settings)?,
            has_secret,
            admin_events: admin_events(&events)?,
            last_attempt_at: last_attempt_at.as_deref().map(parse_ts).transpose()?,
            last_ok,
            last_error_code,
            last_error,
            created_at: parse_ts(&created_at)?,
            updated_at: parse_ts(&updated_at)?,
        })
    }
}

/// A channel to create.
#[derive(Debug, Clone, Copy)]
pub struct NewChannel<'a> {
    pub kind: ChannelKind,
    pub name: &'a str,
    pub enabled: bool,
    pub settings: &'a Value,
    /// Sealed SMTP password or webhook URL (required for webhooks).
    pub secret: Option<&'a StoredCredential>,
    pub admin_events: &'a [NotificationKind],
}

/// Changes to a channel; `None` keeps a field.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChannelUpdate<'a> {
    pub name: Option<&'a str>,
    pub enabled: Option<bool>,
    pub settings: Option<&'a Value>,
    /// `Some(None)` removes the secret (email without login only).
    pub secret: Option<Option<&'a StoredCredential>>,
    pub admin_events: Option<&'a [NotificationKind]>,
}

/// What the worker needs to send on a channel.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelConnection {
    pub id: i64,
    pub kind: ChannelKind,
    pub name: String,
    pub settings: Value,
    pub secret: Option<StoredCredential>,
}

/// A delivery that is due.
#[derive(Debug, Clone, PartialEq)]
pub struct DueDelivery {
    pub id: i64,
    /// Attempts made so far.
    pub attempts: u32,
    pub notification: NotificationRecord,
    pub channel: ChannelConnection,
    /// Member notifications by email: the member's address, if the member
    /// is active and has one.
    pub member_email: Option<String>,
}

/// Error code of deliveries dropped because their channel was disabled.
pub const CODE_CHANNEL_DISABLED: &str = "channel_disabled";

fn name_conflict(err: sqlx::Error) -> DbError {
    if let sqlx::Error::Database(db) = &err
        && db.is_unique_violation()
    {
        return DbError::ChannelTaken;
    }
    DbError::Sqlx(err)
}

fn secret_of(ciphertext: Option<Vec<u8>>, recipient: Option<String>) -> Option<StoredCredential> {
    match (ciphertext, recipient) {
        (Some(ciphertext), Some(recipient)) => Some(StoredCredential {
            ciphertext,
            recipient,
            class: SecretClass::ServiceCredential,
        }),
        _ => None,
    }
}

/// Insert notification `kind` for `member_id` (or the admin stream) unless
/// `dedup_key` was used before, and plan its deliveries. Returns the new
/// notification's id.
async fn insert(
    conn: &mut SqliteConnection,
    member_id: Option<i64>,
    kind: NotificationKind,
    payload: &Payload,
    dedup_key: &str,
    now: DateTime<Utc>,
) -> Result<Option<i64>, DbError> {
    let inserted: Option<(i64,)> = sqlx::query_as(
        "INSERT INTO notification (member_id, kind, payload, dedup_key, created_at)
         SELECT ?1, ?2, ?3, ?4, ?5
         WHERE NOT EXISTS (SELECT 1 FROM notification WHERE member_id IS ?1 AND dedup_key = ?4)
         RETURNING id",
    )
    .bind(member_id)
    .bind(kind.as_str())
    .bind(serde_json::to_string(payload)?)
    .bind(dedup_key)
    .bind(ts(now))
    .fetch_optional(&mut *conn)
    .await?;
    let Some((id,)) = inserted else {
        return Ok(None);
    };
    // Members: by email if they opted in for this kind, are active, have an
    // address and an SMTP server is set up. Admins: every enabled channel that takes this kind (an
    // email channel only with an admin address).
    let sql = if member_id.is_some() {
        "INSERT INTO notification_delivery (notification_id, channel_id, state, next_attempt_at,
             created_at, updated_at)
         SELECT ?1, c.id, 'pending', ?4, ?4, ?4 FROM notification_channel c
         JOIN member_notification_pref p ON p.member_id = ?2 AND p.kind = ?3 AND p.email = 1
         JOIN member m ON m.id = ?2 AND m.status = 'active' AND m.email IS NOT NULL
         WHERE c.kind = 'email' AND c.enabled = 1"
    } else {
        "INSERT INTO notification_delivery (notification_id, channel_id, state, next_attempt_at,
             created_at, updated_at)
         SELECT ?1, c.id, 'pending', ?4, ?4, ?4 FROM notification_channel c
         WHERE ?2 IS NULL AND c.enabled = 1
           AND EXISTS (SELECT 1 FROM json_each(c.admin_events) e WHERE e.value = ?3)
           AND (c.kind <> 'email'
                OR coalesce(json_extract(c.settings, '$.adminAddress'), '') <> '')"
    };
    sqlx::query(sql)
        .bind(id)
        .bind(member_id)
        .bind(kind.as_str())
        .bind(ts(now))
        .execute(&mut *conn)
        .await?;
    Ok(Some(id))
}

/// The title of a movie or series catalog item.
async fn title(conn: &mut SqliteConnection, item: i64) -> Result<(String, Option<i64>), DbError> {
    Ok(
        sqlx::query_as("SELECT title, year FROM catalog_item WHERE id = ?")
            .bind(item)
            .fetch_one(&mut *conn)
            .await?,
    )
}

/// The title a music notification names: "Artist – Album" (several albums
/// joined by commas), with the album's year when it is one album (spec
/// 0006: payload title "Artist – Album", no new payload field).
fn album_title(artist: &str, albums: &[(String, Option<i64>)]) -> (String, Option<i64>) {
    match albums {
        [] => (artist.to_owned(), None),
        [(album, year)] => (format!("{artist} – {album}"), *year),
        many => {
            let names: Vec<&str> = many.iter().map(|(t, _)| t.as_str()).collect();
            (format!("{artist} – {}", names.join(", ")), None)
        }
    }
}

/// Notify about request `id` moving from `before` (`None`: just created) to
/// `after`. Called by the requests repository in the transaction that
/// records the change.
pub(crate) async fn request_transition(
    conn: &mut SqliteConnection,
    id: i64,
    before: Option<RequestState>,
    after: RequestState,
    now: DateTime<Utc>,
) -> Result<(), DbError> {
    match rules::for_transition(before, after) {
        Some(kind) => request_notification(conn, id, kind, now).await,
        None => Ok(()),
    }
}

/// Tell the member that `follows.check` requested a new season of a
/// followed series in their name (request `id`, just created).
pub(crate) async fn found_by_follow(
    conn: &mut SqliteConnection,
    id: i64,
    now: DateTime<Utc>,
) -> Result<(), DbError> {
    request_notification(conn, id, NotificationKind::FollowRequested, now).await
}

/// Tell the member that `follows.check` found more new scenes for their
/// performer or studio follow than the weekly cap allows (spec 0007 Q4):
/// once per follow and `week` of the follow. The payload names the
/// followed performer or studio as `title`; the category keeps it behind
/// the gate in Starr and neutral on every channel (Q8).
pub(crate) async fn follow_capped(
    conn: &mut SqliteConnection,
    follow: &crate::follows::FollowRecord,
    week: i64,
    now: DateTime<Utc>,
) -> Result<bool, DbError> {
    let payload = Payload {
        title: follow.title.clone(),
        year: None,
        category: follow.category.as_str().to_owned(),
        request_id: None,
        acquisition_id: None,
        seasons: vec![],
        reason: None,
        member: None,
    };
    let key = format!("follow:{}:capped:{week}", follow.id);
    Ok(insert(
        conn,
        Some(follow.member_id),
        NotificationKind::FollowCapped,
        &payload,
        &key,
        now,
    )
    .await?
    .is_some())
}

/// Write notification `kind` about request `id` (once per request and kind).
async fn request_notification(
    conn: &mut SqliteConnection,
    id: i64,
    kind: NotificationKind,
    now: DateTime<Utc>,
) -> Result<(), DbError> {
    type Row = (i64, i64, String, Option<String>, Option<String>, String);
    let (member_id, item, category, reject_reason, revoke_reason, username): Row = sqlx::query_as(
        "SELECT r.member_id, r.catalog_item_id, r.category, r.reject_reason,
                    r.revoke_reason, m.username
             FROM request r JOIN member m ON m.id = r.member_id WHERE r.id = ?",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    // Which items the notification is about, as a JSON list of decisions.
    let decisions = match kind {
        NotificationKind::RequestRejected => r#"["rejected"]"#,
        NotificationKind::RequestRevoked => r#"["revoked"]"#,
        NotificationKind::ApprovalPending => r#"["pending_approval"]"#,
        NotificationKind::FollowRequested => r#"["pending_approval","approved"]"#,
        _ => r#"["approved"]"#,
    };
    let seasons: Vec<(i64,)> = sqlx::query_as(
        "SELECT DISTINCT season_number FROM request_item
         WHERE request_id = ?
           AND decision IN (SELECT value FROM json_each(?))
           AND season_number IS NOT NULL
         ORDER BY season_number",
    )
    .bind(id)
    .bind(decisions)
    .fetch_all(&mut *conn)
    .await?;
    let (mut title, mut year) = title(conn, item).await?;
    let albums: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT c.title, c.year FROM request_item i JOIN catalog_item c ON c.id = i.catalog_item_id
         WHERE i.request_id = ? AND c.kind = 'album'
           AND i.decision IN (SELECT value FROM json_each(?))
         ORDER BY i.id",
    )
    .bind(id)
    .bind(decisions)
    .fetch_all(&mut *conn)
    .await?;
    if !albums.is_empty() {
        (title, year) = album_title(&title, &albums);
    }
    let admin = kind.is_admin();
    let payload = Payload {
        title,
        year,
        category,
        request_id: Some(id),
        acquisition_id: None,
        seasons: seasons
            .into_iter()
            .filter_map(|(n,)| u32::try_from(n).ok())
            .collect(),
        reason: match kind {
            NotificationKind::RequestRejected => reject_reason,
            NotificationKind::RequestRevoked => revoke_reason,
            _ => None,
        },
        member: admin.then_some(username),
    };
    let key = format!("request:{id}:{}", kind.as_str());
    insert(
        conn,
        (!admin).then_some(member_id),
        kind,
        &payload,
        &key,
        now,
    )
    .await?;
    Ok(())
}

/// Tell the member that Starr needs their password once for an account on
/// instance `service_id` (`service_name`), at password `generation` (ADR
/// 0010). Once per member, instance and generation; by email only if the
/// member opted in for this kind.
pub(crate) async fn account_activate(
    conn: &mut SqliteConnection,
    member_id: i64,
    service_id: i64,
    service_name: &str,
    generation: i64,
    now: DateTime<Utc>,
) -> Result<bool, DbError> {
    let payload = Payload {
        title: service_name.to_owned(),
        year: None,
        category: "account".to_owned(),
        request_id: None,
        acquisition_id: None,
        seasons: vec![],
        reason: None,
        member: None,
    };
    let key = format!("account:{service_id}:activate:{generation}");
    Ok(insert(
        conn,
        Some(member_id),
        NotificationKind::AccountActivate,
        &payload,
        &key,
        now,
    )
    .await?
    .is_some())
}

/// The admin payload about acquisition `id`: its title (the series for a
/// season, "Artist – Album" for an album) and `reason`.
async fn acquisition_payload(
    conn: &mut SqliteConnection,
    id: i64,
    reason: &str,
) -> Result<Payload, DbError> {
    type Row = (
        String,
        i64,
        Option<i64>,
        Option<i64>,
        String,
        String,
        Option<i64>,
    );
    let (category, item, parent, season, kind, unit_title, unit_year): Row = sqlx::query_as(
        "SELECT a.category, c.id, c.parent_id, c.season_number, c.kind, c.title, c.year
             FROM acquisition a JOIN catalog_item c ON c.id = a.catalog_item_id WHERE a.id = ?",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    let (mut title, mut year) = title(conn, parent.unwrap_or(item)).await?;
    if kind == "album" {
        (title, year) = album_title(&title, &[(unit_title, unit_year)]);
    }
    Ok(Payload {
        title,
        year,
        category,
        request_id: None,
        acquisition_id: Some(id),
        seasons: season
            .and_then(|n| u32::try_from(n).ok())
            .into_iter()
            .collect(),
        reason: Some(reason.to_owned()),
        member: None,
    })
}

/// Tell the admins that acquisition `id` failed for good with `reason`.
/// Called in the transaction that failed it; one notification per failure,
/// however many requests share the acquisition.
pub(crate) async fn acquisition_failed(
    conn: &mut SqliteConnection,
    id: i64,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<(), DbError> {
    let payload = acquisition_payload(conn, id, reason).await?;
    let key = format!("acquisition:{id}:failed:{}", ts(now));
    insert(
        conn,
        None,
        NotificationKind::AcquisitionFailed,
        &payload,
        &key,
        now,
    )
    .await?;
    Ok(())
}

/// Tell the admins that the source blocked the import of acquisition `id`
/// with `message`. Called in the transaction that records the observation;
/// once per acquisition, however often the sync sees it blocked again (as
/// long as the notification is kept: after housekeeping removed it, a
/// title still blocked is reported anew).
pub(crate) async fn import_blocked(
    conn: &mut SqliteConnection,
    id: i64,
    message: &str,
    now: DateTime<Utc>,
) -> Result<(), DbError> {
    let key = format!("acquisition:{id}:import_blocked");
    // Every sync sees a blocked import again: skip building the payload.
    let reported: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM notification WHERE member_id IS NULL AND dedup_key = ?")
            .bind(&key)
            .fetch_optional(&mut *conn)
            .await?;
    if reported.is_some() {
        return Ok(());
    }
    let payload = acquisition_payload(conn, id, message).await?;
    insert(
        conn,
        None,
        NotificationKind::ImportBlocked,
        &payload,
        &key,
        now,
    )
    .await?;
    Ok(())
}

impl Database {
    /// The stream's notifications, newest first, older than `before_id` if
    /// given.
    pub async fn list_notifications(
        &self,
        stream: Stream,
        before_id: Option<i64>,
        limit: u32,
    ) -> Result<Vec<NotificationRecord>, DbError> {
        self.list_notifications_without(stream, before_id, limit, None)
            .await
    }

    /// [`Self::list_notifications`] leaving out the notifications of
    /// `without` (the private category behind a closed gate, spec 0007),
    /// filtered in SQL.
    pub async fn list_notifications_without(
        &self,
        stream: Stream,
        before_id: Option<i64>,
        limit: u32,
        without: Option<Category>,
    ) -> Result<Vec<NotificationRecord>, DbError> {
        let rows: Vec<NotificationRow> = sqlx::query_as(
            "SELECT id, member_id, kind, payload, created_at, read_at FROM notification
             WHERE member_id IS ? AND (?2 IS NULL OR id < ?2)
               AND (?4 IS NULL OR json_extract(payload, '$.category') IS NOT ?4)
             ORDER BY id DESC LIMIT ?3",
        )
        .bind(stream.member_id())
        .bind(before_id)
        .bind(i64::from(limit))
        .bind(without.map(Category::as_str))
        .fetch_all(self.pool())
        .await?;
        crate::readable(rows.into_iter().map(TryInto::try_into))
    }

    /// How many of the stream's notifications are unread.
    pub async fn unread_notifications(&self, stream: Stream) -> Result<u64, DbError> {
        self.unread_notifications_without(stream, None).await
    }

    /// [`Self::unread_notifications`] not counting those of `without`.
    pub async fn unread_notifications_without(
        &self,
        stream: Stream,
        without: Option<Category>,
    ) -> Result<u64, DbError> {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM notification WHERE member_id IS ?1 AND read_at IS NULL
               AND (?2 IS NULL OR json_extract(payload, '$.category') IS NOT ?2)",
        )
        .bind(stream.member_id())
        .bind(without.map(Category::as_str))
        .fetch_one(self.pool())
        .await?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Mark the stream's notifications read: those with `ids`, or all if
    /// `ids` is `None`. Ids of other streams are ignored. Returns how many
    /// changed.
    pub async fn mark_notifications_read(
        &self,
        stream: Stream,
        ids: Option<&[i64]>,
        now: DateTime<Utc>,
    ) -> Result<u64, DbError> {
        self.mark_notifications_read_without(stream, ids, None, now)
            .await
    }

    /// [`Self::mark_notifications_read`] leaving those of `without` alone
    /// (a page that does not show them cannot mark them read).
    pub async fn mark_notifications_read_without(
        &self,
        stream: Stream,
        ids: Option<&[i64]>,
        without: Option<Category>,
        now: DateTime<Utc>,
    ) -> Result<u64, DbError> {
        let ids = ids.map(serde_json::to_string).transpose()?;
        let res = sqlx::query(
            "UPDATE notification SET read_at = ?1
             WHERE member_id IS ?2 AND read_at IS NULL
               AND (?3 IS NULL OR id IN (SELECT value FROM json_each(?3)))
               AND (?4 IS NULL OR json_extract(payload, '$.category') IS NOT ?4)",
        )
        .bind(ts(now))
        .bind(stream.member_id())
        .bind(ids)
        .bind(without.map(Category::as_str))
        .execute(self.pool())
        .await?;
        Ok(res.rows_affected())
    }

    /// The member kinds the member also gets by email.
    pub async fn email_preferences(
        &self,
        member_id: i64,
    ) -> Result<Vec<NotificationKind>, DbError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT kind FROM member_notification_pref WHERE member_id = ? AND email = 1
             ORDER BY kind",
        )
        .bind(member_id)
        .fetch_all(self.pool())
        .await?;
        crate::readable(rows.iter().map(|(k,)| kind(k)))
    }

    /// Replace the member kinds the member gets by email.
    pub async fn set_email_preferences(
        &self,
        member_id: i64,
        email: &[NotificationKind],
    ) -> Result<(), DbError> {
        let mut tx = self.begin_write().await?;
        // Only the kinds this release knows: preferences for kinds a newer
        // release added stay (ADR 0012).
        let member_kinds: Vec<&str> = NotificationKind::MEMBER
            .iter()
            .map(|k| k.as_str())
            .collect();
        sqlx::query(
            "DELETE FROM member_notification_pref
             WHERE member_id = ? AND kind IN (SELECT value FROM json_each(?))",
        )
        .bind(member_id)
        .bind(serde_json::to_string(&member_kinds)?)
        .execute(&mut *tx)
        .await?;
        for kind in NotificationKind::MEMBER
            .iter()
            .filter(|k| email.contains(k))
        {
            sqlx::query(
                "INSERT INTO member_notification_pref (member_id, kind, email) VALUES (?, ?, 1)",
            )
            .bind(member_id)
            .bind(kind.as_str())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Tell the member that Starr needs their password once for an account
    /// on instance `service_id` (ADR 0010): once per member, instance and
    /// password `generation`. `true` if this call notified.
    pub async fn notify_account_activate(
        &self,
        member_id: i64,
        service_id: i64,
        service_name: &str,
        generation: i64,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let notified = account_activate(
            &mut tx,
            member_id,
            service_id,
            service_name,
            generation,
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(notified)
    }

    /// Tell the admins about account `remote_id` of member `username` on
    /// instance `service_id` (`kind`: `AccountPrivileged` or
    /// `AccountLocked`): once while the notification is kept, however often
    /// a plan sees it again. `true` if this call notified.
    pub async fn notify_account_attention(
        &self,
        kind: NotificationKind,
        service_id: i64,
        service_name: &str,
        remote_id: &str,
        username: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        if !matches!(
            kind,
            NotificationKind::AccountPrivileged | NotificationKind::AccountLocked
        ) {
            return Err(DbError::InvalidData(format!(
                "{} is not an account notification",
                kind.as_str()
            )));
        }
        let payload = Payload {
            title: service_name.to_owned(),
            year: None,
            category: "account".to_owned(),
            request_id: None,
            acquisition_id: None,
            seasons: vec![],
            reason: None,
            member: Some(username.to_owned()),
        };
        let key = format!("account:{service_id}:{remote_id}:{}", kind.as_str());
        let mut tx = self.begin_write().await?;
        let notified = insert(&mut tx, None, kind, &payload, &key, now)
            .await?
            .is_some();
        tx.commit().await?;
        Ok(notified)
    }

    /// Whether an enabled email channel exists (members can opt in).
    pub async fn email_channel_enabled(&self) -> Result<bool, DbError> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM notification_channel WHERE kind = 'email' AND enabled = 1",
        )
        .fetch_optional(self.pool())
        .await?;
        Ok(row.is_some())
    }

    pub async fn list_channels(&self) -> Result<Vec<ChannelRecord>, DbError> {
        let rows: Vec<ChannelRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {CHANNEL_COLUMNS} FROM notification_channel ORDER BY kind = 'email' DESC, name"
        )))
        .fetch_all(self.pool())
        .await?;
        crate::readable(rows.into_iter().map(TryInto::try_into))
    }

    pub async fn get_channel(&self, id: i64) -> Result<Option<ChannelRecord>, DbError> {
        let row: Option<ChannelRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {CHANNEL_COLUMNS} FROM notification_channel WHERE id = ?"
        )))
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    /// Create a channel. [`DbError::ChannelTaken`] if the name is used or
    /// an email channel exists already.
    pub async fn create_channel(
        &self,
        new: &NewChannel<'_>,
        now: DateTime<Utc>,
    ) -> Result<ChannelRecord, DbError> {
        let (id,): (i64,) = sqlx::query_as(
            "INSERT INTO notification_channel (kind, name, enabled, settings, secret_ciphertext,
                 secret_recipient, admin_events, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(new.kind.as_str())
        .bind(new.name)
        .bind(new.enabled)
        .bind(serde_json::to_string(new.settings)?)
        .bind(new.secret.map(|s| s.ciphertext.as_slice()))
        .bind(new.secret.map(|s| s.recipient.as_str()))
        .bind(events_json(new.admin_events)?)
        .bind(ts(now))
        .bind(ts(now))
        .fetch_one(self.pool())
        .await
        .map_err(name_conflict)?;
        self.get_channel(id)
            .await?
            .ok_or_else(|| DbError::InvalidData("channel vanished".to_owned()))
    }

    /// Change a channel. `None` if it does not exist. Disabling it fails
    /// its pending deliveries ([`CODE_CHANNEL_DISABLED`]).
    pub async fn update_channel(
        &self,
        id: i64,
        update: &ChannelUpdate<'_>,
        now: DateTime<Utc>,
    ) -> Result<Option<ChannelRecord>, DbError> {
        let mut tx = self.begin_write().await?;
        let settings = update.settings.map(serde_json::to_string).transpose()?;
        let events = match update.admin_events {
            None => None,
            Some(events) => {
                let stored: Option<(String,)> =
                    sqlx::query_as("SELECT admin_events FROM notification_channel WHERE id = ?")
                        .bind(id)
                        .fetch_optional(&mut *tx)
                        .await?;
                let mut names: Vec<String> = serde_json::from_str(&events_json(events)?)?;
                if let Some((stored,)) = stored {
                    names.extend(unknown_events(&stored)?);
                }
                names.sort_unstable();
                names.dedup();
                Some(serde_json::to_string(&names)?)
            }
        };
        let (set_secret, secret) = match update.secret {
            None => (false, None),
            Some(secret) => (true, secret),
        };
        let res = sqlx::query(
            "UPDATE notification_channel SET
                 name = coalesce(?1, name),
                 enabled = coalesce(?2, enabled),
                 settings = coalesce(?3, settings),
                 secret_ciphertext = iif(?4, ?5, secret_ciphertext),
                 secret_recipient = iif(?4, ?6, secret_recipient),
                 admin_events = coalesce(?7, admin_events),
                 updated_at = ?8
             WHERE id = ?9",
        )
        .bind(update.name)
        .bind(update.enabled)
        .bind(settings)
        .bind(set_secret)
        .bind(secret.map(|s| s.ciphertext.as_slice()))
        .bind(secret.map(|s| s.recipient.as_str()))
        .bind(events)
        .bind(ts(now))
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(name_conflict)?;
        if res.rows_affected() == 0 {
            return Ok(None);
        }
        if update.enabled == Some(false) {
            sqlx::query(
                "UPDATE notification_delivery SET state = 'failed', next_attempt_at = NULL,
                     error_code = ?1, last_error = 'the channel was disabled', updated_at = ?2
                 WHERE channel_id = ?3 AND state = 'pending'",
            )
            .bind(CODE_CHANNEL_DISABLED)
            .bind(ts(now))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        self.get_channel(id).await
    }

    /// Delete a channel and its deliveries. `false` if it did not exist.
    pub async fn delete_channel(&self, id: i64) -> Result<bool, DbError> {
        let res = sqlx::query("DELETE FROM notification_channel WHERE id = ?")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// What the worker needs to send on channel `id` (also when disabled,
    /// for a test).
    pub async fn channel_connection(&self, id: i64) -> Result<Option<ChannelConnection>, DbError> {
        type Row = (String, String, String, Option<Vec<u8>>, Option<String>);
        let row: Option<Row> = sqlx::query_as(
            "SELECT kind, name, settings, secret_ciphertext, secret_recipient
                 FROM notification_channel WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        row.map(|(kind, name, settings, ciphertext, recipient)| {
            Ok(ChannelConnection {
                id,
                kind: channel_kind(&kind)?,
                name,
                settings: serde_json::from_str(&settings)?,
                secret: secret_of(ciphertext, recipient),
            })
        })
        .transpose()
    }

    /// Pending deliveries on enabled channels that are due, oldest first.
    pub async fn due_deliveries(
        &self,
        now: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<DueDelivery>, DbError> {
        type Row = (
            i64,
            i64,
            i64,
            Option<i64>,
            String,
            String,
            String,
            Option<String>,
            i64,
            String,
            String,
            String,
            Option<Vec<u8>>,
            Option<String>,
            Option<String>,
        );
        // Deliveries of kinds a newer release added are not this release's
        // to send; filtered here so they never fill the page (ADR 0012).
        let known = known_kinds_json()?;
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT d.id, d.attempts, n.id, n.member_id, n.kind, n.payload, n.created_at,
                 n.read_at, c.id, c.kind, c.name, c.settings, c.secret_ciphertext,
                 c.secret_recipient, iif(m.status = 'active', m.email, NULL)
             FROM notification_delivery d
             JOIN notification n ON n.id = d.notification_id
             JOIN notification_channel c ON c.id = d.channel_id
             LEFT JOIN member m ON m.id = n.member_id
             WHERE d.state = 'pending' AND c.enabled = 1
               AND (d.next_attempt_at IS NULL OR d.next_attempt_at <= ?1)
               AND n.kind IN (SELECT value FROM json_each(?3))
               AND c.kind IN (SELECT value FROM json_each(?4))
             ORDER BY d.next_attempt_at, d.id LIMIT ?2",
        )
        .bind(ts(now))
        .bind(i64::from(limit))
        .bind(&known.0)
        .bind(&known.1)
        .fetch_all(self.pool())
        .await?;
        crate::readable(rows.into_iter().map(|row| {
            let (
                id,
                attempts,
                n_id,
                member_id,
                n_kind,
                payload,
                created_at,
                read_at,
                c_id,
                c_kind,
                c_name,
                settings,
                ciphertext,
                recipient,
                member_email,
            ) = row;
            Ok(DueDelivery {
                id,
                attempts: u32::try_from(attempts).unwrap_or(u32::MAX),
                notification: (n_id, member_id, n_kind, payload, created_at, read_at).try_into()?,
                channel: ChannelConnection {
                    id: c_id,
                    kind: channel_kind(&c_kind)?,
                    name: c_name,
                    settings: serde_json::from_str(&settings)?,
                    secret: secret_of(ciphertext, recipient),
                },
                member_email,
            })
        }))
    }

    /// Whether any delivery on an enabled channel is due.
    pub async fn has_due_deliveries(&self, now: DateTime<Utc>) -> Result<bool, DbError> {
        let known = known_kinds_json()?;
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM notification_delivery d
             JOIN notification n ON n.id = d.notification_id
             JOIN notification_channel c ON c.id = d.channel_id
             WHERE d.state = 'pending' AND c.enabled = 1
               AND (d.next_attempt_at IS NULL OR d.next_attempt_at <= ?1)
               AND n.kind IN (SELECT value FROM json_each(?2))
               AND c.kind IN (SELECT value FROM json_each(?3))
             LIMIT 1",
        )
        .bind(ts(now))
        .bind(&known.0)
        .bind(&known.1)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.is_some())
    }

    /// Delivery `id` was sent.
    pub async fn record_delivered(&self, id: i64, now: DateTime<Utc>) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE notification_delivery SET state = 'delivered', attempts = attempts + 1,
                 next_attempt_at = NULL, error_code = NULL, last_error = NULL,
                 delivered_at = ?1, updated_at = ?1
             WHERE id = ?2 AND state = 'pending'",
        )
        .bind(ts(now))
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Delivery `id` failed with taxonomy `code` and readable `reason`: due
    /// again at `retry_at`, or failed for good without one.
    pub async fn record_delivery_failure(
        &self,
        id: i64,
        code: &str,
        reason: &str,
        retry_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE notification_delivery SET state = iif(?1 IS NULL, 'failed', 'pending'),
                 attempts = attempts + 1, next_attempt_at = ?1, error_code = ?2,
                 last_error = ?3, updated_at = ?4
             WHERE id = ?5 AND state = 'pending'",
        )
        .bind(retry_at.map(ts))
        .bind(code)
        .bind(reason)
        .bind(ts(now))
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Delivery `id` was throttled: due again at `at`, without using up an
    /// attempt (the channel asked us to wait, nothing failed).
    pub async fn postpone_delivery(
        &self,
        id: i64,
        code: &str,
        reason: &str,
        at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE notification_delivery SET next_attempt_at = ?1, error_code = ?2,
                 last_error = ?3, updated_at = ?4
             WHERE id = ?5 AND state = 'pending'",
        )
        .bind(ts(at))
        .bind(code)
        .bind(reason)
        .bind(ts(now))
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Record the outcome of the latest send (delivery or test) on channel
    /// `id`: `None` for success, else taxonomy code and readable reason.
    pub async fn record_channel_result(
        &self,
        id: i64,
        error: Option<(&str, &str)>,
        now: DateTime<Utc>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE notification_channel SET last_attempt_at = ?1, last_ok = ?2,
                 last_error_code = ?3, last_error = ?4
             WHERE id = ?5",
        )
        .bind(ts(now))
        .bind(error.is_none())
        .bind(error.map(|(code, _)| code))
        .bind(error.map(|(_, reason)| reason))
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Delete notifications (and their deliveries) created before `cutoff`.
    pub async fn delete_notifications_before(&self, cutoff: DateTime<Utc>) -> Result<u64, DbError> {
        let res = sqlx::query("DELETE FROM notification WHERE created_at < ?")
            .bind(ts(cutoff))
            .execute(self.pool())
            .await?;
        Ok(res.rows_affected())
    }

    /// Delete the notifications of `category` (both streams, read or not,
    /// with their deliveries) created before `cutoff`: the private
    /// category's shorter retention (spec 0007 Q13).
    pub async fn delete_category_notifications_before(
        &self,
        category: Category,
        cutoff: DateTime<Utc>,
    ) -> Result<u64, DbError> {
        let res = sqlx::query(
            "DELETE FROM notification
             WHERE created_at < ?1 AND json_extract(payload, '$.category') = ?2",
        )
        .bind(ts(cutoff))
        .bind(category.as_str())
        .execute(self.pool())
        .await?;
        Ok(res.rows_affected())
    }
}

#[cfg(test)]
#[path = "notifications_tests.rs"]
mod tests;
