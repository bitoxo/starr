//! Service accounts and the account plan (spec 0005 slice 3): the admin's
//! decision per member × consume instance (undecided / create / linked /
//! none), the account Starr owns there (D4), and the last plan per
//! instance. Nothing here talks to a service; linking is a Starr-side
//! record that the planner turns into steps.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, Transaction};
use starr_core::provisioning::{Decision, Expected, Origin, PasswordUse};

use crate::{Database, DbError, ts};

/// One stored record (member × instance).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceAccountRow {
    pub member_id: i64,
    pub service_instance_id: i64,
    pub decision: Decision,
    pub origin: Option<Origin>,
    pub remote_id: Option<String>,
    /// The name there when it was linked or created.
    pub linked_username: Option<String>,
    pub password: Option<PasswordUse>,
    /// The password generation Starr last wrote into the account (ADR
    /// 0010); `None`: never written.
    pub password_written: Option<i64>,
    /// A password write of this generation was started and its result not
    /// recorded (a crash or cancellation in between): never written again
    /// automatically, so devices are not signed out twice.
    pub password_attempt: Option<i64>,
    /// What Starr last left the account as (drift baseline, D13).
    pub expected: Option<Expected>,
    pub decided_at: DateTime<Utc>,
    /// Admin username; `None` for decisions Starr made itself.
    pub decided_by: Option<String>,
}

/// What an admin decides for a member on an instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionChange<'a> {
    Undecided,
    Create,
    NoAccount,
    /// Link this account (adopted, password deferred).
    Link {
        remote_id: &'a str,
    },
}

/// Why a link was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRefusal {
    /// No such account in the instance's inventory.
    NotFound,
    /// Administrator in the service (D2).
    Privileged,
    /// Marked ignored; unignore it first.
    Ignored,
    /// Gone from the service.
    Gone,
    /// Another member owns it.
    Taken { member_id: i64 },
    /// Named by an invitation made before adopting needed an elevation
    /// (spec 0005 M10 2): accepting it does not adopt; an admin confirms
    /// the link (with elevation) instead.
    Unconfirmed,
}

/// The result of [`Database::decide_service_account`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecideOutcome {
    /// Stored; `previous` is the row before (`None`: there was none).
    Changed {
        row: ServiceAccountRow,
        previous: Option<ServiceAccountRow>,
    },
    /// The same decision was already stored.
    Unchanged(ServiceAccountRow),
    Refused(LinkRefusal),
    /// The member owns an account Starr created there; changing that needs
    /// the replace and delete rules of later slices (D8, D16).
    ProvisionedAccount,
}

/// The result of [`Database::request_password_switch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchOutcome {
    /// The switch waits for the worker (now, or already before).
    Requested,
    /// The account already uses the Starr password.
    AlreadyStarr,
    /// No account of the member there.
    NoAccount,
}

/// An account Starr created that never got a password ("needs you").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountWithoutPassword {
    pub service_id: i64,
    pub service_name: String,
    pub member_id: i64,
    pub username: String,
    pub remote_id: String,
}

/// One account an invitation names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetAccount {
    pub service_id: i64,
    pub remote_id: String,
    /// The name there when the invitation was created (for display).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// The admin who named it had just re-authenticated (M10 2). Presets
    /// stored without it (before this field existed) are not adopted on
    /// acceptance: the member stays undecided there for an admin.
    #[serde(default)]
    pub confirmed: bool,
}

/// The stored plan of one instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPlan {
    pub service_instance_id: i64,
    pub computed_at: DateTime<Utc>,
    pub complete: bool,
    pub fingerprint: String,
    /// `starr_core::provisioning::Plan` as JSON.
    pub plan: String,
    /// `starr_core::provisioning::Counts` as JSON.
    pub counts: String,
}

#[derive(sqlx::FromRow)]
struct Row {
    member_id: i64,
    service_instance_id: i64,
    decision: String,
    origin: Option<String>,
    remote_id: Option<String>,
    linked_username: Option<String>,
    password: Option<String>,
    password_generation_written: Option<i64>,
    password_attempt_generation: Option<i64>,
    expected_enabled: Option<bool>,
    expected_libraries: Option<String>,
    decided_at: DateTime<Utc>,
    decided_by: Option<String>,
}

impl TryFrom<Row> for ServiceAccountRow {
    type Error = DbError;

    fn try_from(row: Row) -> Result<Self, DbError> {
        Ok(Self {
            decision: Decision::parse(&row.decision)
                .ok_or_else(|| crate::unknown("account decision", &row.decision))?,
            origin: row
                .origin
                .as_deref()
                .map(|o| Origin::parse(o).ok_or_else(|| crate::unknown("account origin", o)))
                .transpose()?,
            password: row
                .password
                .as_deref()
                .map(|p| PasswordUse::parse(p).ok_or_else(|| crate::unknown("account password", p)))
                .transpose()?,
            member_id: row.member_id,
            service_instance_id: row.service_instance_id,
            remote_id: row.remote_id,
            linked_username: row.linked_username,
            password_written: row.password_generation_written,
            password_attempt: row.password_attempt_generation,
            expected: match row.expected_enabled {
                Some(enabled) => Some(Expected {
                    enabled,
                    libraries: row
                        .expected_libraries
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()?,
                }),
                None => None,
            },
            decided_at: row.decided_at,
            decided_by: row.decided_by,
        })
    }
}

const COLUMNS: &str =
    "member_id, service_instance_id, decision, origin, remote_id, linked_username,
     password, password_generation_written, password_attempt_generation, expected_enabled,
     expected_libraries, decided_at, decided_by";

async fn row_in(
    tx: &mut Transaction<'_, Sqlite>,
    member_id: i64,
    instance_id: i64,
) -> Result<Option<Row>, DbError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM service_account WHERE member_id = ? AND service_instance_id = ?"
    );
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(member_id)
        .bind(instance_id)
        .fetch_optional(&mut **tx)
        .await?)
}

/// Check that `remote_id` may be linked to `member_id`; the account's name.
async fn linkable(
    tx: &mut Transaction<'_, Sqlite>,
    member_id: i64,
    instance_id: i64,
    remote_id: &str,
) -> Result<Result<String, LinkRefusal>, DbError> {
    let account: Option<(String, bool, bool, bool)> = sqlx::query_as(
        "SELECT username, privileged, ignored_at IS NOT NULL, gone_since IS NOT NULL
         FROM remote_account WHERE service_instance_id = ? AND remote_id = ?",
    )
    .bind(instance_id)
    .bind(remote_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((username, privileged, ignored, gone)) = account else {
        return Ok(Err(LinkRefusal::NotFound));
    };
    if privileged {
        return Ok(Err(LinkRefusal::Privileged));
    }
    if ignored {
        return Ok(Err(LinkRefusal::Ignored));
    }
    if gone {
        return Ok(Err(LinkRefusal::Gone));
    }
    let owner: Option<(i64,)> = sqlx::query_as(
        "SELECT member_id FROM service_account
         WHERE service_instance_id = ? AND remote_id = ? AND member_id <> ?",
    )
    .bind(instance_id)
    .bind(remote_id)
    .bind(member_id)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((member_id,)) = owner {
        return Ok(Err(LinkRefusal::Taken { member_id }));
    }
    Ok(Ok(username))
}

/// Insert or replace the row; returns it.
async fn upsert(
    tx: &mut Transaction<'_, Sqlite>,
    member_id: i64,
    instance_id: i64,
    decision: Decision,
    link: Option<(&str, &str)>,
    by: Option<&str>,
    now: DateTime<Utc>,
) -> Result<ServiceAccountRow, DbError> {
    let (origin, remote_id, username, password) = match link {
        Some((remote_id, username)) => (
            Some(Origin::Adopted.as_str()),
            Some(remote_id),
            Some(username),
            Some(PasswordUse::Deferred.as_str()),
        ),
        None => (None, None, None, None),
    };
    let sql = format!(
        "INSERT INTO service_account (member_id, service_instance_id, decision, origin, remote_id,
             linked_username, password, decided_at, decided_by)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (member_id, service_instance_id) DO UPDATE SET
             decision = excluded.decision, origin = excluded.origin,
             remote_id = excluded.remote_id, linked_username = excluded.linked_username,
             password = excluded.password, password_generation_written = NULL,
             password_attempt_generation = NULL, expected_enabled = NULL,
             expected_libraries = NULL, decided_at = excluded.decided_at,
             decided_by = excluded.decided_by, last_error = NULL,
             last_error_detail = NULL, last_error_at = NULL
         RETURNING {COLUMNS}"
    );
    let row: Row = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(member_id)
        .bind(instance_id)
        .bind(decision.as_str())
        .bind(origin)
        .bind(remote_id)
        .bind(username)
        .bind(password)
        .bind(ts(now))
        .bind(by)
        .fetch_one(&mut **tx)
        .await?;
    row.try_into()
}

impl Database {
    /// Every readable record of one instance (rows with a value this
    /// binary does not know are skipped: their members count as undecided,
    /// so Starr does nothing for them).
    pub async fn service_accounts(
        &self,
        instance_id: i64,
    ) -> Result<Vec<ServiceAccountRow>, DbError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM service_account WHERE service_instance_id = ? ORDER BY member_id"
        );
        let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(instance_id)
            .fetch_all(self.pool())
            .await?;
        crate::readable(rows.into_iter().map(TryInto::try_into))
    }

    /// Every readable record of one member.
    pub async fn member_service_accounts(
        &self,
        member_id: i64,
    ) -> Result<Vec<ServiceAccountRow>, DbError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM service_account WHERE member_id = ? ORDER BY service_instance_id"
        );
        let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(member_id)
            .fetch_all(self.pool())
            .await?;
        crate::readable(rows.into_iter().map(TryInto::try_into))
    }

    /// Store an admin's decision for a member on an instance, in one
    /// transaction. A link is checked against the inventory: the account
    /// exists, is not privileged, ignored or gone, and no other member owns
    /// it. Leaving an adopted account releases it (nothing is written to
    /// the service); an account Starr created cannot be left yet.
    pub async fn decide_service_account(
        &self,
        member_id: i64,
        instance_id: i64,
        change: DecisionChange<'_>,
        by: &str,
        now: DateTime<Utc>,
    ) -> Result<DecideOutcome, DbError> {
        let mut tx = self.begin_write().await?;
        // A row a newer release wrote fails here: never overwritten.
        let previous: Option<ServiceAccountRow> = row_in(&mut tx, member_id, instance_id)
            .await?
            .map(TryInto::try_into)
            .transpose()?;
        let (decision, remote_id) = match change {
            DecisionChange::Undecided => (Decision::Undecided, None),
            DecisionChange::Create => (Decision::Create, None),
            DecisionChange::NoAccount => (Decision::None, None),
            DecisionChange::Link { remote_id } => (Decision::Linked, Some(remote_id)),
        };
        if let Some(prev) = &previous {
            if prev.decision == decision && prev.remote_id.as_deref() == remote_id {
                tx.rollback().await?;
                return Ok(DecideOutcome::Unchanged(prev.clone()));
            }
            if prev.origin == Some(Origin::Provisioned) {
                tx.rollback().await?;
                return Ok(DecideOutcome::ProvisionedAccount);
            }
        }
        let link = match remote_id {
            Some(remote_id) => match linkable(&mut tx, member_id, instance_id, remote_id).await? {
                Ok(username) => Some((remote_id, username)),
                Err(refusal) => {
                    tx.rollback().await?;
                    return Ok(DecideOutcome::Refused(refusal));
                }
            },
            None => None,
        };
        let row = upsert(
            &mut tx,
            member_id,
            instance_id,
            decision,
            link.as_ref().map(|(id, name)| (*id, name.as_str())),
            Some(by),
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(DecideOutcome::Changed { row, previous })
    }

    /// Decisions for a member created just now (M3): `create` on every
    /// instance in `create_on` (consume instances with accounts), except
    /// those where `links` names an account to adopt. A link that is no
    /// longer possible (gone, ignored, privileged, owned by someone else)
    /// leaves the member undecided there for the admin. Existing rows stay.
    /// Returns the links that were refused.
    pub async fn initial_service_accounts(
        &self,
        member_id: i64,
        create_on: &[i64],
        links: &[PresetAccount],
        now: DateTime<Utc>,
    ) -> Result<Vec<(PresetAccount, LinkRefusal)>, DbError> {
        let mut tx = self.begin_write().await?;
        let mut refused = Vec::new();
        for preset in links {
            if row_in(&mut tx, member_id, preset.service_id)
                .await?
                .is_some()
            {
                continue;
            }
            let linkable = if preset.confirmed {
                linkable(&mut tx, member_id, preset.service_id, &preset.remote_id).await?
            } else {
                Err(LinkRefusal::Unconfirmed)
            };
            match linkable {
                Ok(username) => {
                    upsert(
                        &mut tx,
                        member_id,
                        preset.service_id,
                        Decision::Linked,
                        Some((&preset.remote_id, &username)),
                        None,
                        now,
                    )
                    .await?;
                }
                Err(refusal) => {
                    upsert(
                        &mut tx,
                        member_id,
                        preset.service_id,
                        Decision::Undecided,
                        None,
                        None,
                        now,
                    )
                    .await?;
                    refused.push((preset.clone(), refusal));
                }
            }
        }
        for &instance in create_on {
            if row_in(&mut tx, member_id, instance).await?.is_some() {
                continue;
            }
            upsert(
                &mut tx,
                member_id,
                instance,
                Decision::Create,
                None,
                None,
                now,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(refused)
    }

    /// Record the account Starr just created for a member decided
    /// "create" (D4: created by Starr is the evidence of ownership), with
    /// the password generation written into it (`None`: the create did not
    /// finish and no password of the member reached it). `false` if the
    /// decision changed meanwhile or the member already owns an account
    /// there (the account then stays unowned and shows as a conflict).
    pub async fn record_created_account(
        &self,
        instance_id: i64,
        member_id: i64,
        remote_id: &str,
        username: &str,
        written: Option<i64>,
    ) -> Result<bool, DbError> {
        let done = sqlx::query(
            "UPDATE service_account SET origin = ?, remote_id = ?, linked_username = ?,
                 password = ?, password_generation_written = ?
             WHERE member_id = ? AND service_instance_id = ? AND decision = ?
               AND remote_id IS NULL
               AND NOT EXISTS (SELECT 1 FROM service_account o
                               WHERE o.service_instance_id = ? AND o.remote_id = ?)",
        )
        .bind(Origin::Provisioned.as_str())
        .bind(remote_id)
        .bind(username)
        .bind(PasswordUse::Starr.as_str())
        .bind(written)
        .bind(member_id)
        .bind(instance_id)
        .bind(Decision::Create.as_str())
        .bind(instance_id)
        .bind(remote_id)
        .execute(self.pool())
        .await?
        .rows_affected()
            == 1;
        Ok(done)
    }

    /// Record that the worker wrote password generation `generation` into
    /// the member's account `remote_id`: it uses the Starr password from
    /// now on (a pending switch is done; an account Starr had locked with a
    /// random password is unlocked, also an adopted one). `false` if the
    /// account is not the member's any more or keeps the old password (the
    /// member never asked to switch). One transaction.
    pub async fn record_password_written(
        &self,
        instance_id: i64,
        member_id: i64,
        remote_id: &str,
        generation: i64,
    ) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let written = sqlx::query(
            "UPDATE service_account SET password = ?, password_generation_written = ?,
                 password_attempt_generation = NULL
             WHERE member_id = ? AND service_instance_id = ? AND remote_id = ?
               AND (password IN (?, ?)
                    OR EXISTS (SELECT 1 FROM remote_account r
                               WHERE r.service_instance_id = ? AND r.remote_id = ?
                                 AND r.locked_by_starr = 1))",
        )
        .bind(PasswordUse::Starr.as_str())
        .bind(generation)
        .bind(member_id)
        .bind(instance_id)
        .bind(remote_id)
        .bind(PasswordUse::Starr.as_str())
        .bind(PasswordUse::Pending.as_str())
        .bind(instance_id)
        .bind(remote_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if written {
            sqlx::query(
                "UPDATE remote_account SET locked_by_starr = 0
                 WHERE service_instance_id = ? AND remote_id = ?",
            )
            .bind(instance_id)
            .bind(remote_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(written)
    }

    /// Record that Starr locked the member's account `remote_id` by writing
    /// a random password nobody knows (D9: a service without a disable
    /// flag, or a create that did not finish there). Only the member's
    /// password unlocks it ([`Self::record_password_written`]). The lock
    /// belongs to the account: unlinking and linking it again keeps it.
    /// `false` if the account is not the member's any more.
    /// An account the inventory does not list yet (a create a moment ago)
    /// is added with the name Starr gave it, seen `now`; the next scan
    /// fills in the rest.
    pub async fn record_locked_by_starr(
        &self,
        instance_id: i64,
        member_id: i64,
        remote_id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        Ok(sqlx::query(
            "INSERT INTO remote_account (service_instance_id, remote_id, username, enabled,
                 privileged, locked_by_service, libraries, first_seen, last_seen, locked_by_starr)
             SELECT a.service_instance_id, a.remote_id, coalesce(a.linked_username, ''), 1,
                 0, 0, NULL, ?4, ?4, 1
             FROM service_account a
             WHERE a.service_instance_id = ?1 AND a.remote_id = ?2 AND a.member_id = ?3
             ON CONFLICT (service_instance_id, remote_id) DO UPDATE SET locked_by_starr = 1",
        )
        .bind(instance_id)
        .bind(remote_id)
        .bind(member_id)
        .bind(ts(now))
        .execute(self.pool())
        .await?
        .rows_affected()
            == 1)
    }

    /// The member asks to use their Starr password on an adopted account
    /// (M2): `deferred` becomes `pending` until the worker has written it.
    /// Already pending counts as done (a repeated request renews the
    /// handoff only).
    pub async fn request_password_switch(
        &self,
        member_id: i64,
        instance_id: i64,
    ) -> Result<SwitchOutcome, DbError> {
        let mut tx = self.begin_write().await?;
        let Some(row) = row_in(&mut tx, member_id, instance_id).await? else {
            tx.rollback().await?;
            return Ok(SwitchOutcome::NoAccount);
        };
        let row: ServiceAccountRow = row.try_into()?;
        let outcome = match (row.remote_id.as_deref(), row.password) {
            (None, _) => SwitchOutcome::NoAccount,
            (Some(_), Some(PasswordUse::Starr)) => SwitchOutcome::AlreadyStarr,
            (Some(_), Some(PasswordUse::Pending)) => SwitchOutcome::Requested,
            (Some(_), Some(PasswordUse::Deferred) | None) => {
                sqlx::query(
                    "UPDATE service_account SET password = ?
                     WHERE member_id = ? AND service_instance_id = ?",
                )
                .bind(PasswordUse::Pending.as_str())
                .bind(member_id)
                .bind(instance_id)
                .execute(&mut *tx)
                .await?;
                SwitchOutcome::Requested
            }
        };
        tx.commit().await?;
        Ok(outcome)
    }

    /// Undo a switch request whose password could not be handed over
    /// (`pending` back to `deferred`; a switch already written stays).
    pub async fn cancel_password_switch(
        &self,
        member_id: i64,
        instance_id: i64,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE service_account SET password = ?
             WHERE member_id = ? AND service_instance_id = ? AND password = ?",
        )
        .bind(PasswordUse::Deferred.as_str())
        .bind(member_id)
        .bind(instance_id)
        .bind(PasswordUse::Pending.as_str())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Whether entering the member's password now would let Starr do
    /// anything with it (ADR 0010: no handoff without a use): an instance
    /// with account writes switched on (live or paused) where the member is
    /// decided "create" without an account yet, or owns an account whose
    /// Starr password is due (another generation, never written, or a
    /// switch the member asked for), or that Starr locked with a random
    /// password (D9).
    pub async fn password_handoff_wanted(&self, member_id: i64) -> Result<bool, DbError> {
        let (wanted,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (
                 SELECT 1 FROM service_account a
                 JOIN service_instance i ON i.id = a.service_instance_id
                 JOIN member m ON m.id = a.member_id
                 WHERE a.member_id = ? AND i.accounts_live_since IS NOT NULL
                   AND ((a.decision = ? AND a.remote_id IS NULL)
                        OR EXISTS (SELECT 1 FROM remote_account r
                                   WHERE r.service_instance_id = a.service_instance_id
                                     AND r.remote_id = a.remote_id AND r.locked_by_starr = 1)
                        OR (a.remote_id IS NOT NULL AND a.password = ?)
                        OR (a.remote_id IS NOT NULL AND a.password = ?
                            AND a.password_generation_written IS NOT m.password_generation)))",
        )
        .bind(member_id)
        .bind(Decision::Create.as_str())
        .bind(PasswordUse::Pending.as_str())
        .bind(PasswordUse::Starr.as_str())
        .fetch_one(self.pool())
        .await?;
        Ok(wanted)
    }

    /// Accounts Starr created but never wrote a password into (a create
    /// that did not finish), for "needs you": they stay disabled until the
    /// member's password reaches them.
    pub async fn accounts_without_password(&self) -> Result<Vec<AccountWithoutPassword>, DbError> {
        let rows: Vec<(i64, String, i64, String, String)> = sqlx::query_as(
            "SELECT a.service_instance_id, i.name, a.member_id, m.username, a.remote_id
             FROM service_account a
             JOIN service_instance i ON i.id = a.service_instance_id
             JOIN member m ON m.id = a.member_id
             WHERE a.origin = ? AND a.password = ? AND a.password_generation_written IS NULL
               AND a.remote_id IS NOT NULL
             ORDER BY i.name, m.username",
        )
        .bind(Origin::Provisioned.as_str())
        .bind(PasswordUse::Starr.as_str())
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(service_id, service_name, member_id, username, remote_id)| {
                    AccountWithoutPassword {
                        service_id,
                        service_name,
                        member_id,
                        username,
                        remote_id,
                    }
                },
            )
            .collect())
    }

    /// The member who owns `remote_id` on an instance, if any.
    pub async fn account_owner(
        &self,
        instance_id: i64,
        remote_id: &str,
    ) -> Result<Option<i64>, DbError> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT member_id FROM service_account WHERE service_instance_id = ? AND remote_id = ?",
        )
        .bind(instance_id)
        .bind(remote_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|(id,)| id))
    }

    /// Store the plan of an instance, replacing the previous one. A plan
    /// computed before the stored one is dropped (two planners racing).
    /// `false` if the instance no longer exists.
    pub async fn store_account_plan(&self, plan: &StoredPlan) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let exists: Option<(i64,)> = sqlx::query_as("SELECT id FROM service_instance WHERE id = ?")
            .bind(plan.service_instance_id)
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO account_plan (service_instance_id, computed_at, complete, fingerprint,
                 plan, counts)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (service_instance_id) DO UPDATE SET
                 computed_at = excluded.computed_at, complete = excluded.complete,
                 fingerprint = excluded.fingerprint, plan = excluded.plan,
                 counts = excluded.counts
             WHERE excluded.computed_at >= account_plan.computed_at",
        )
        .bind(plan.service_instance_id)
        .bind(ts(plan.computed_at))
        .bind(plan.complete)
        .bind(&plan.fingerprint)
        .bind(&plan.plan)
        .bind(&plan.counts)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// The stored plan of one instance.
    pub async fn account_plan(&self, instance_id: i64) -> Result<Option<StoredPlan>, DbError> {
        let row: Option<(i64, DateTime<Utc>, bool, String, String, String)> = sqlx::query_as(
            "SELECT service_instance_id, computed_at, complete, fingerprint, plan, counts
             FROM account_plan WHERE service_instance_id = ?",
        )
        .bind(instance_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(
            |(service_instance_id, computed_at, complete, fingerprint, plan, counts)| StoredPlan {
                service_instance_id,
                computed_at,
                complete,
                fingerprint,
                plan,
                counts,
            },
        ))
    }

    /// Whether any plan is stored (a replan would drop stale ones).
    pub async fn has_account_plans(&self) -> Result<bool, DbError> {
        let (any,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM account_plan)")
            .fetch_one(self.pool())
            .await?;
        Ok(any)
    }

    /// Drop the plans of instances that no longer have accounts (no
    /// consume role left); `keep` are the instances still planned.
    pub async fn prune_account_plans(&self, keep: &[i64]) -> Result<(), DbError> {
        let keep = serde_json::to_string(keep)?;
        sqlx::query(
            "DELETE FROM account_plan
             WHERE service_instance_id NOT IN (SELECT value FROM json_each(?))",
        )
        .bind(keep)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::members::NewMember;
    use crate::remote_accounts::{AccountScan, ObservedAccount};
    use crate::services::{NewServiceInstance, StoredCredential};
    use crate::test_support::t0;

    async fn instance(db: &Database, name: &str) -> i64 {
        db.create_service(
            &NewServiceInstance {
                adapter_type: "jellyfin",
                name,
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

    async fn member(db: &Database, name: &str) -> i64 {
        let email = format!("{name}@example.org");
        db.create_member(
            &NewMember {
                username: name,
                email: &email,
                display_name: name,
                password_hash: None,
            },
            &[],
            t0(),
        )
        .await
        .unwrap()
    }

    fn observed(id: &str, name: &str) -> ObservedAccount {
        ObservedAccount {
            remote_id: id.to_owned(),
            username: name.to_owned(),
            enabled: true,
            privileged: false,
            locked_by_service: false,
            libraries: None,
        }
    }

    async fn scanned(db: &Database, id: i64, accounts: Vec<ObservedAccount>) {
        db.store_account_scan(
            id,
            &AccountScan {
                complete: true,
                observed: accounts,
                missing: vec![],
            },
            t0(),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn links_are_checked_against_the_inventory() {
        let db = Database::open_in_memory().await.unwrap();
        let jf = instance(&db, "Jellyfin").await;
        let alex = member(&db, "alex").await;
        let sam = member(&db, "sam").await;
        let admin = ObservedAccount {
            privileged: true,
            ..observed("r-admin", "admin")
        };
        scanned(
            &db,
            jf,
            vec![observed("r-alex", "Alexander"), observed("r-x", "x"), admin],
        )
        .await;
        db.set_remote_account_ignored(jf, "r-x", Some("root"), t0())
            .await
            .unwrap();
        let link = |id| DecisionChange::Link { remote_id: id };

        for (remote, refusal) in [
            ("nope", LinkRefusal::NotFound),
            ("r-admin", LinkRefusal::Privileged),
            ("r-x", LinkRefusal::Ignored),
        ] {
            assert_eq!(
                db.decide_service_account(alex, jf, link(remote), "root", t0())
                    .await
                    .unwrap(),
                DecideOutcome::Refused(refusal)
            );
        }
        let DecideOutcome::Changed { row, previous } = db
            .decide_service_account(alex, jf, link("r-alex"), "root", t0())
            .await
            .unwrap()
        else {
            panic!("not linked");
        };
        assert_eq!(previous, None);
        assert_eq!(row.decision, Decision::Linked);
        assert_eq!(row.origin, Some(Origin::Adopted));
        assert_eq!(row.password, Some(PasswordUse::Deferred));
        assert_eq!(row.linked_username.as_deref(), Some("Alexander"));
        assert_eq!(row.decided_by.as_deref(), Some("root"));
        // Repeating it changes nothing.
        assert!(matches!(
            db.decide_service_account(alex, jf, link("r-alex"), "root", t0())
                .await
                .unwrap(),
            DecideOutcome::Unchanged(_)
        ));
        // Sam cannot take Alex's account.
        assert_eq!(
            db.decide_service_account(sam, jf, link("r-alex"), "root", t0())
                .await
                .unwrap(),
            DecideOutcome::Refused(LinkRefusal::Taken { member_id: alex })
        );
        assert_eq!(db.account_owner(jf, "r-alex").await.unwrap(), Some(alex));

        // Deciding "none" releases the account.
        let DecideOutcome::Changed { row, previous } = db
            .decide_service_account(alex, jf, DecisionChange::NoAccount, "root", t0())
            .await
            .unwrap()
        else {
            panic!("not changed");
        };
        assert_eq!(previous.unwrap().remote_id.as_deref(), Some("r-alex"));
        assert_eq!(
            (row.remote_id, row.origin, row.password),
            (None, None, None)
        );
        assert_eq!(db.account_owner(jf, "r-alex").await.unwrap(), None);
        assert_eq!(db.service_accounts(jf).await.unwrap().len(), 1);
        assert_eq!(db.member_service_accounts(sam).await.unwrap(), vec![]);
    }

    #[tokio::test]
    async fn accounts_starr_created_cannot_be_left_yet() {
        let db = Database::open_in_memory().await.unwrap();
        let jf = instance(&db, "Jellyfin").await;
        let alex = member(&db, "alex").await;
        sqlx::query(
            "INSERT INTO service_account (member_id, service_instance_id, decision, origin,
                 remote_id, password, decided_at)
             VALUES (?, ?, 'create', 'provisioned', 'r1', 'starr', '2026-11-01T00:00:00.000Z')",
        )
        .bind(alex)
        .bind(jf)
        .execute(db.pool())
        .await
        .unwrap();
        assert_eq!(
            db.decide_service_account(alex, jf, DecisionChange::NoAccount, "root", t0())
                .await
                .unwrap(),
            DecideOutcome::ProvisionedAccount
        );
    }

    #[tokio::test]
    async fn new_members_get_create_or_their_preset_links() {
        let db = Database::open_in_memory().await.unwrap();
        let jf = instance(&db, "Jellyfin").await;
        let emby = instance(&db, "Emby").await;
        let alex = member(&db, "alex").await;
        let sam = member(&db, "sam").await;
        scanned(&db, jf, vec![observed("r-alex", "alex")]).await;
        db.decide_service_account(
            sam,
            jf,
            DecisionChange::Link {
                remote_id: "r-alex",
            },
            "root",
            t0(),
        )
        .await
        .unwrap();
        // Alex's preset on Emby names an account that does not exist.
        let presets = [
            PresetAccount {
                service_id: jf,
                remote_id: "r-alex".into(),
                username: None,
                confirmed: true,
            },
            PresetAccount {
                service_id: emby,
                remote_id: "gone".into(),
                username: None,
                confirmed: true,
            },
        ];
        let refused = db
            .initial_service_accounts(alex, &[jf, emby], &presets, t0())
            .await
            .unwrap();
        assert_eq!(
            refused,
            vec![
                (presets[0].clone(), LinkRefusal::Taken { member_id: sam }),
                (presets[1].clone(), LinkRefusal::NotFound),
            ]
        );
        let rows = db.member_service_accounts(alex).await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.decision).collect::<Vec<_>>(),
            vec![Decision::Undecided, Decision::Undecided],
            "refused presets leave the member undecided, never create"
        );
        assert!(rows.iter().all(|r| r.decided_by.is_none()));

        let mia = member(&db, "mia").await;
        db.decide_service_account(sam, jf, DecisionChange::NoAccount, "root", t0())
            .await
            .unwrap();
        let refused = db
            .initial_service_accounts(mia, &[jf, emby], &presets[..1], t0())
            .await
            .unwrap();
        assert_eq!(refused, vec![]);
        let rows = db.member_service_accounts(mia).await.unwrap();
        assert_eq!(rows[0].decision, Decision::Linked);
        assert_eq!(rows[0].remote_id.as_deref(), Some("r-alex"));
        assert_eq!(rows[1].decision, Decision::Create);

        // A preset stored before adopting needed an elevation (M10 2) is
        // not adopted on acceptance, and nothing is created there either.
        let old = PresetAccount {
            confirmed: false,
            ..presets[0].clone()
        };
        let parsed: PresetAccount =
            serde_json::from_str(r#"{"serviceId":1,"remoteId":"r-alex"}"#).unwrap();
        assert!(!parsed.confirmed, "stored without the field");
        let noah = member(&db, "noah").await;
        db.decide_service_account(mia, jf, DecisionChange::NoAccount, "root", t0())
            .await
            .unwrap();
        let refused = db
            .initial_service_accounts(noah, &[jf], std::slice::from_ref(&old), t0())
            .await
            .unwrap();
        assert_eq!(refused, vec![(old, LinkRefusal::Unconfirmed)]);
        let rows = db.member_service_accounts(noah).await.unwrap();
        assert_eq!(rows[0].decision, Decision::Undecided);
    }

    #[tokio::test]
    async fn unknown_decisions_are_skipped_and_never_overwritten() {
        let db = Database::open_in_memory().await.unwrap();
        let jf = instance(&db, "Jellyfin").await;
        let alex = member(&db, "alex").await;
        sqlx::query(
            "INSERT INTO service_account (member_id, service_instance_id, decision, decided_at)
             VALUES (?, ?, 'later', '2026-11-01T00:00:00.000Z')",
        )
        .bind(alex)
        .bind(jf)
        .execute(db.pool())
        .await
        .unwrap();
        assert_eq!(db.service_accounts(jf).await.unwrap(), vec![]);
        assert!(matches!(
            db.decide_service_account(alex, jf, DecisionChange::Create, "root", t0())
                .await,
            Err(DbError::UnknownValue { .. })
        ));
        db.initial_service_accounts(alex, &[jf], &[], t0())
            .await
            .unwrap();
        let (decision,): (String,) =
            sqlx::query_as("SELECT decision FROM service_account WHERE member_id = ?")
                .bind(alex)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(decision, "later");
    }

    #[tokio::test]
    async fn plans_are_stored_per_instance_and_never_go_back_in_time() {
        let db = Database::open_in_memory().await.unwrap();
        let jf = instance(&db, "Jellyfin").await;
        let emby = instance(&db, "Emby").await;
        let plan = |at, fp: &str| StoredPlan {
            service_instance_id: jf,
            computed_at: at,
            complete: true,
            fingerprint: fp.to_owned(),
            plan: r#"{"complete":true,"entries":[]}"#.to_owned(),
            counts: "{}".to_owned(),
        };
        let later = t0() + chrono::TimeDelta::minutes(1);
        assert!(db.store_account_plan(&plan(later, "new")).await.unwrap());
        assert!(db.store_account_plan(&plan(t0(), "old")).await.unwrap());
        let stored = db.account_plan(jf).await.unwrap().unwrap();
        assert_eq!(
            (stored.fingerprint.as_str(), stored.computed_at),
            ("new", later)
        );
        assert_eq!(db.account_plan(emby).await.unwrap(), None);
        db.prune_account_plans(&[emby]).await.unwrap();
        assert_eq!(db.account_plan(jf).await.unwrap(), None);
        let gone = StoredPlan {
            service_instance_id: 999,
            ..plan(t0(), "x")
        };
        assert!(!db.store_account_plan(&gone).await.unwrap());
    }

    #[tokio::test]
    async fn created_accounts_and_password_writes_are_recorded_per_member() {
        let db = Database::open_in_memory().await.unwrap();
        let jf = instance(&db, "Jellyfin").await;
        let alex = member(&db, "alex").await;
        let sam = member(&db, "sam").await;
        scanned(&db, jf, vec![observed("r-sam", "sam")]).await;
        db.initial_service_accounts(alex, &[jf], &[], t0())
            .await
            .unwrap();
        db.decide_service_account(
            sam,
            jf,
            DecisionChange::Link { remote_id: "r-sam" },
            "root",
            t0(),
        )
        .await
        .unwrap();
        // Nothing wanted while account writes were never switched on.
        assert!(!db.password_handoff_wanted(alex).await.unwrap());
        sqlx::query("UPDATE service_instance SET accounts_live_since = ?")
            .bind(ts(t0()))
            .execute(db.pool())
            .await
            .unwrap();
        assert!(
            db.password_handoff_wanted(alex).await.unwrap(),
            "a create waits"
        );
        assert!(!db.password_handoff_wanted(sam).await.unwrap(), "deferred");

        // An unfinished create: owned, Starr password, never written.
        assert!(
            db.record_created_account(jf, alex, "r-alex", "alex", None)
                .await
                .unwrap()
        );
        assert!(
            !db.record_created_account(jf, alex, "r-other", "alex", Some(1))
                .await
                .unwrap(),
            "only once"
        );
        assert!(
            !db.record_created_account(jf, sam, "r-new", "sam", Some(1))
                .await
                .unwrap(),
            "sam was not decided create"
        );
        let missing = db.accounts_without_password().await.unwrap();
        assert_eq!(missing.len(), 1);
        assert_eq!(
            (missing[0].member_id, missing[0].remote_id.as_str()),
            (alex, "r-alex")
        );
        assert!(
            db.record_password_written(jf, alex, "r-alex", 1)
                .await
                .unwrap()
        );
        assert_eq!(db.accounts_without_password().await.unwrap(), vec![]);
        let row = db.member_service_accounts(alex).await.unwrap().remove(0);
        assert_eq!(
            (row.origin, row.password, row.password_written),
            (Some(Origin::Provisioned), Some(PasswordUse::Starr), Some(1))
        );

        // Sam keeps the old password until asking to switch (M2).
        assert!(
            !db.record_password_written(jf, sam, "r-sam", 1)
                .await
                .unwrap()
        );
        assert_eq!(
            db.request_password_switch(sam, jf).await.unwrap(),
            SwitchOutcome::Requested
        );
        assert_eq!(
            db.request_password_switch(sam, jf).await.unwrap(),
            SwitchOutcome::Requested
        );
        assert!(db.password_handoff_wanted(sam).await.unwrap());
        assert!(
            !db.record_password_written(jf, alex, "r-sam", 2)
                .await
                .unwrap(),
            "not alex's"
        );
        assert!(
            db.record_password_written(jf, sam, "r-sam", 2)
                .await
                .unwrap()
        );
        assert_eq!(
            db.request_password_switch(sam, jf).await.unwrap(),
            SwitchOutcome::AlreadyStarr
        );
        let other = instance(&db, "Emby").await;
        assert_eq!(
            db.request_password_switch(sam, other).await.unwrap(),
            SwitchOutcome::NoAccount
        );
        // Relinking forgets the written generation.
        let row = db.member_service_accounts(sam).await.unwrap().remove(0);
        assert_eq!(row.password_written, Some(2));
    }

    /// D9 with two members: a scramble locks the account (also an adopted
    /// one), makes the member's password wanted, and the next password
    /// written unlocks it; the other member's account is untouched.
    #[tokio::test]
    async fn starr_locks_and_the_member_password_unlocks() {
        let db = Database::open_in_memory().await.unwrap();
        let nd = instance(&db, "Navidrome").await;
        let alex = member(&db, "alex").await;
        let sam = member(&db, "sam").await;
        scanned(
            &db,
            nd,
            vec![observed("r-alex", "alex"), observed("r-sam", "sam")],
        )
        .await;
        for (m, remote_id) in [(alex, "r-alex"), (sam, "r-sam")] {
            db.decide_service_account(m, nd, DecisionChange::Link { remote_id }, "root", t0())
                .await
                .unwrap();
        }
        sqlx::query("UPDATE service_instance SET accounts_live_since = ?")
            .bind(ts(t0()))
            .execute(db.pool())
            .await
            .unwrap();
        assert!(!db.password_handoff_wanted(alex).await.unwrap(), "deferred");

        assert!(
            db.record_locked_by_starr(nd, alex, "r-alex", t0())
                .await
                .unwrap()
        );
        assert!(
            !db.record_locked_by_starr(nd, alex, "r-sam", t0())
                .await
                .unwrap(),
            "not alex's"
        );
        let row = async |m| db.member_service_accounts(m).await.unwrap().remove(0);
        let locked = async |remote: &str| {
            db.remote_accounts(nd)
                .await
                .unwrap()
                .into_iter()
                .find(|r| r.remote_id == remote)
                .unwrap()
                .locked_by_starr
        };
        assert!(locked("r-alex").await);
        assert!(!locked("r-sam").await);
        assert!(db.password_handoff_wanted(alex).await.unwrap());
        assert!(!db.password_handoff_wanted(sam).await.unwrap());
        // Not an unfinished create: not "without a password".
        assert_eq!(db.accounts_without_password().await.unwrap(), vec![]);

        // Adopted with the old password, yet the member's password lands.
        assert!(
            db.record_password_written(nd, alex, "r-alex", 1)
                .await
                .unwrap()
        );
        let alex_row = row(alex).await;
        assert!(!locked("r-alex").await);
        assert_eq!(
            (alex_row.password, alex_row.password_written),
            (Some(PasswordUse::Starr), Some(1))
        );
        assert_eq!(row(sam).await.password, Some(PasswordUse::Deferred));

        // The lock belongs to the account: unlinking and linking it again
        // (here to alex's neighbour mia) keeps it, so the new owner is asked
        // for a password instead of "uses your old password".
        db.record_locked_by_starr(nd, sam, "r-sam", t0())
            .await
            .unwrap();
        db.decide_service_account(sam, nd, DecisionChange::Undecided, "root", t0())
            .await
            .unwrap();
        assert!(locked("r-sam").await);
        assert!(
            !db.record_locked_by_starr(nd, sam, "r-sam", t0())
                .await
                .unwrap(),
            "no longer sam's"
        );
        let mia = member(&db, "mia").await;
        db.decide_service_account(
            mia,
            nd,
            DecisionChange::Link { remote_id: "r-sam" },
            "root",
            t0(),
        )
        .await
        .unwrap();
        assert!(locked("r-sam").await);
        assert!(db.password_handoff_wanted(mia).await.unwrap());
        // A scan leaves the lock alone.
        scanned(
            &db,
            nd,
            vec![observed("r-alex", "alex"), observed("r-sam", "sam")],
        )
        .await;
        assert!(locked("r-sam").await);
    }
}
