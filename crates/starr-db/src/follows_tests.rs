#![allow(clippy::unwrap_used)]

use chrono::TimeDelta;
use sqlx::migrate::Migrator;
use starr_core::adapters::SecretClass;
use starr_core::members::{Entitlement, RequestMode};

use super::*;
use crate::members::NewMember;
use crate::requests::{Actor, EventTrigger, NewRequest, RequestFollow, SourceTarget, Unit};
use crate::services::{NewServiceInstance, StoredCredential};
use crate::test_support::t0;
use crate::{EMBEDDED, migrator};

const SYSTEM: Actor<'static> = Actor {
    trigger: EventTrigger::System,
    name: "follows.check",
};

struct Fixture {
    db: Database,
    mia: i64,
    noah: i64,
    sonarr: i64,
    series: i64,
    /// Season items by number 1, 2, 3.
    seasons: Vec<i64>,
}

async fn member(db: &Database, name: &str) -> i64 {
    db.create_member(
        &NewMember {
            username: name,
            email: &format!("{name}@example.com"),
            display_name: name,
            password_hash: None,
        },
        &[] as &[Entitlement],
        t0(),
    )
    .await
    .unwrap()
}

async fn scalar(db: &Database, sql: String) -> i64 {
    let (id,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(ts(t0()))
        .fetch_one(db.pool())
        .await
        .unwrap();
    id
}

/// A series with seasons 1–3 (1 and 2 with an air date) in `db`.
async fn series(db: &Database) -> (i64, Vec<i64>) {
    let series = scalar(
        db,
        "INSERT INTO catalog_item (kind, tmdb_id, tvdb_id, title, created_at, updated_at)
         VALUES ('series', 1399, 121361, 'Game of Thrones', ?1, ?1) RETURNING id"
            .to_owned(),
    )
    .await;
    let mut seasons = Vec::new();
    for (n, aired) in [(1, "\"2011-04-17\""), (2, "\"2012-04-01\""), (3, "null")] {
        seasons.push(
            scalar(
                db,
                format!(
                    "INSERT INTO catalog_item (kind, parent_id, season_number, title, metadata,
                         metadata_source, metadata_fetched_at, metadata_expires_at,
                         created_at, updated_at)
                     VALUES ('season', {series}, {n}, 'Season {n}',
                         '{{\"episodeCount\": 10, \"airDate\": {aired}}}',
                         'tmdb', ?1, ?1, ?1, ?1) RETURNING id"
                ),
            )
            .await,
        );
    }
    (series, seasons)
}

async fn fixture() -> Fixture {
    let db = Database::open_in_memory().await.unwrap();
    let mia = member(&db, "mia").await;
    let noah = member(&db, "noah").await;
    let sonarr = db
        .create_service(
            &NewServiceInstance {
                adapter_type: "sonarr",
                name: "sonarr",
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
        .id;
    let (series, seasons) = series(&db).await;
    crate::test_support::keys_from_columns(&db).await;
    Fixture {
        db,
        mia,
        noah,
        sonarr,
        series,
        seasons,
    }
}

impl Fixture {
    fn source(&self) -> SourceTarget {
        SourceTarget {
            category: Category::Series,
            service_instance_id: self.sonarr,
            quality_profile_id: 6,
            root_folder_path: "/tv".into(),
            minimum_availability: None,
            metadata_profile_id: None,
        }
    }

    async fn request(
        &self,
        member: i64,
        season: u32,
        follow: RequestFollow,
    ) -> Result<crate::requests::Created, DbError> {
        let source = self.source();
        self.db
            .create_request(
                &NewRequest {
                    member_id: member,
                    catalog_item_id: self.series,
                    follow,
                    units: &[Unit {
                        catalog_item_id: self.seasons[season as usize - 1],
                        season_number: Some(season),
                    }],
                    mode: RequestMode::Approval,
                    source: &source,
                },
                SYSTEM,
                t0(),
            )
            .await
    }

    async fn mine(&self, member: i64) -> Vec<FollowRecord> {
        self.db.member_follows(member).await.unwrap()
    }
}

#[tokio::test]
async fn follows_are_personal_and_one_per_member_and_series() {
    let f = fixture().await;
    let (mia, created) =
        f.db.follow(f.mia, f.series, Category::Series, Cutoff::Seasons(2), t0())
            .await
            .unwrap();
    assert!(created);
    assert_eq!(mia.title, "Game of Thrones");
    assert_eq!(mia.tmdb_id, Some(1399));
    assert_eq!(mia.cutoff, Cutoff::Seasons(2));
    assert_eq!(mia.member_username, "mia");
    let (noah, created) =
        f.db.follow(f.noah, f.series, Category::Series, Cutoff::Seasons(2), t0())
            .await
            .unwrap();
    assert!(
        created,
        "another member follows the same series on their own"
    );
    assert_ne!(mia.id, noah.id);

    // Following again keeps the follow; the cutoff only moves up.
    let (again, created) =
        f.db.follow(f.mia, f.series, Category::Series, Cutoff::Seasons(1), t0())
            .await
            .unwrap();
    assert!(!created);
    assert_eq!((again.id, again.cutoff), (mia.id, Cutoff::Seasons(2)));

    // Each sees only their own; nobody ends another member's follow.
    assert_eq!(f.mine(f.mia).await.len(), 1);
    assert_eq!(f.mine(f.mia).await[0].id, mia.id);
    assert!(!f.db.unfollow(f.noah, mia.id, t0()).await.unwrap());
    assert_eq!(f.mine(f.mia).await.len(), 1);

    assert!(f.db.unfollow(f.mia, mia.id, t0()).await.unwrap());
    assert!(!f.db.unfollow(f.mia, mia.id, t0()).await.unwrap());
    assert_eq!(f.mine(f.mia).await.len(), 0);
    assert_eq!(f.mine(f.noah).await.len(), 1, "Noah still follows");
    assert!(f.db.has_due_follows(t0()).await.unwrap());

    // Following again starts over with a new follow.
    let later = t0() + TimeDelta::days(1);
    let (renewed, created) =
        f.db.follow(f.mia, f.series, Category::Series, Cutoff::Seasons(3), later)
            .await
            .unwrap();
    assert!(created);
    assert_ne!(renewed.id, mia.id);
    assert_eq!(renewed.created_at, later);
}

/// Undo of an unfollow (spec 0008 F8, PRIV-17): the ended follow comes back
/// as it was (id, cutoff, start), only for its member, only shortly after
/// it ended, and never beside a newer follow of the same title.
#[tokio::test]
async fn an_ended_follow_comes_back_as_it_was_shortly_after() {
    let f = fixture().await;
    let (mia, _) =
        f.db.follow(f.mia, f.series, Category::Series, Cutoff::Seasons(2), t0())
            .await
            .unwrap();
    let ended = t0() + TimeDelta::days(3);
    assert!(f.db.unfollow(f.mia, mia.id, ended).await.unwrap());
    let since = ended - TimeDelta::minutes(1);

    // Not another member's, and not one that ended before `since`.
    assert_eq!(
        f.db.restore_follow(f.noah, mia.id, Category::Series, since)
            .await
            .unwrap(),
        Restored::NotFound
    );
    assert_eq!(
        f.db.restore_follow(
            f.mia,
            mia.id,
            Category::Series,
            ended + TimeDelta::seconds(1)
        )
        .await
        .unwrap(),
        Restored::NotFound
    );
    // Not through another category's undo.
    assert_eq!(
        f.db.restore_follow(f.mia, mia.id, Category::Adult, since)
            .await
            .unwrap(),
        Restored::NotFound
    );
    let Restored::Back(back) =
        f.db.restore_follow(f.mia, mia.id, Category::Series, since)
            .await
            .unwrap()
    else {
        panic!("not restored");
    };
    assert_eq!(
        (back.id, back.cutoff, back.created_at),
        (mia.id, Cutoff::Seasons(2), t0())
    );
    assert_eq!(f.mine(f.mia).await.len(), 1);
    // Restoring an active follow keeps it.
    assert_eq!(
        f.db.restore_follow(f.mia, mia.id, Category::Series, since)
            .await
            .unwrap(),
        Restored::Active(back)
    );

    // Followed again meanwhile: the old one stays ended.
    assert!(f.db.unfollow(f.mia, mia.id, ended).await.unwrap());
    let (renewed, _) =
        f.db.follow(f.mia, f.series, Category::Series, Cutoff::Seasons(3), ended)
            .await
            .unwrap();
    assert_eq!(
        f.db.restore_follow(f.mia, mia.id, Category::Series, since)
            .await
            .unwrap(),
        Restored::Superseded
    );
    assert_eq!(f.mine(f.mia).await[0].id, renewed.id);
}

const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";

/// Radiohead with its MBID, and an album of it with its release group.
async fn artist(db: &Database) -> (i64, i64) {
    let artist = scalar(
        db,
        "INSERT INTO catalog_item (kind, title, created_at, updated_at)
         VALUES ('artist', 'Radiohead', ?1, ?1) RETURNING id"
            .to_owned(),
    )
    .await;
    let album = scalar(
        db,
        format!(
            "INSERT INTO catalog_item (kind, parent_id, title, created_at, updated_at)
             VALUES ('album', {artist}, 'OK Computer', ?1, ?1) RETURNING id"
        ),
    )
    .await;
    sqlx::query(
        "INSERT INTO catalog_key (catalog_item_id, scheme, value) VALUES
             (?1, 'mb_artist', ?2), (?3, 'mb_release_group', 'b1392450-e666-3926-a536-22c65f834433')",
    )
    .bind(artist)
    .bind(RADIOHEAD)
    .bind(album)
    .execute(db.pool())
    .await
    .unwrap();
    (artist, album)
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

#[tokio::test]
async fn an_artist_follow_keeps_its_date_and_is_personal() {
    let f = fixture().await;
    let (artist, _) = artist(&f.db).await;
    let (mia, created) =
        f.db.follow(
            f.mia,
            artist,
            Category::Music,
            Cutoff::Date(day(2026, 11, 1)),
            t0(),
        )
        .await
        .unwrap();
    assert!(created);
    assert_eq!(mia.kind, ItemKind::Artist);
    assert_eq!(mia.mb_artist, Some(Uuid::parse_str(RADIOHEAD).unwrap()));
    assert_eq!(mia.tmdb_id, None);
    assert_eq!(mia.cutoff, Cutoff::Date(day(2026, 11, 1)));
    assert_eq!(mia.category, Category::Music);

    // Following again keeps the follow and its date: a later date would
    // drop what was released in between.
    let (again, created) =
        f.db.follow(
            f.mia,
            artist,
            Category::Music,
            Cutoff::Date(day(2026, 12, 24)),
            t0() + TimeDelta::days(53),
        )
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(
        (again.id, again.cutoff),
        (mia.id, Cutoff::Date(day(2026, 11, 1)))
    );

    // Noah's follow of the same artist is his own, with his own date.
    let (noah, created) =
        f.db.follow(
            f.noah,
            artist,
            Category::Music,
            Cutoff::Date(day(2026, 12, 1)),
            t0(),
        )
        .await
        .unwrap();
    assert!(created);
    assert_eq!(noah.cutoff, Cutoff::Date(day(2026, 12, 1)));
    assert_eq!(f.mine(f.mia).await.len(), 1);
    assert!(!f.db.unfollow(f.noah, mia.id, t0()).await.unwrap());

    // A found album leaves the date where it is.
    assert!(
        f.db.follow_checked(mia.id, f.mia, None, t0())
            .await
            .unwrap()
    );
    assert_eq!(
        f.mine(f.mia).await[0].cutoff,
        Cutoff::Date(day(2026, 11, 1))
    );
}

#[tokio::test]
async fn due_follows_are_those_not_checked_since_and_the_longest_unchecked_come_first() {
    let f = fixture().await;
    let (artist, _) = artist(&f.db).await;
    let (series, _) =
        f.db.follow(f.mia, f.series, Category::Series, Cutoff::Seasons(2), t0())
            .await
            .unwrap();
    let (music, _) =
        f.db.follow(
            f.mia,
            artist,
            Category::Music,
            Cutoff::Date(day(2026, 11, 1)),
            t0(),
        )
        .await
        .unwrap();
    let (noahs, _) =
        f.db.follow(
            f.noah,
            artist,
            Category::Music,
            Cutoff::Date(day(2026, 11, 1)),
            t0(),
        )
        .await
        .unwrap();
    let ids = |due: Vec<FollowRecord>| due.into_iter().map(|d| d.id).collect::<Vec<_>>();

    // Never checked: due at once.
    assert!(f.db.has_due_follows(t0()).await.unwrap());
    assert_eq!(ids(f.db.due_follows(t0()).await.unwrap()).len(), 3);

    let morning = t0();
    let noon = t0() + TimeDelta::hours(3);
    f.db.follow_checked(series.id, f.mia, None, noon)
        .await
        .unwrap();
    f.db.follow_checked(music.id, f.mia, None, morning)
        .await
        .unwrap();
    // Before both checks: only Noah's, never checked.
    assert_eq!(
        ids(f
            .db
            .due_follows(morning - TimeDelta::hours(1))
            .await
            .unwrap()),
        [noahs.id]
    );
    // A day later all are due, never checked first, then the oldest check.
    let day_later = noon + TimeDelta::hours(24);
    assert_eq!(
        ids(f.db.due_follows(day_later).await.unwrap()),
        [noahs.id, music.id, series.id]
    );

    // Checked recently and ended ones are not due.
    f.db.follow_checked(noahs.id, f.noah, None, noon)
        .await
        .unwrap();
    f.db.unfollow(f.mia, music.id, noon).await.unwrap();
    assert_eq!(
        ids(f
            .db
            .due_follows(noon - TimeDelta::minutes(1))
            .await
            .unwrap()),
        Vec::<i64>::new()
    );
    assert!(
        !f.db
            .has_due_follows(noon - TimeDelta::minutes(1))
            .await
            .unwrap()
    );
    assert!(f.db.has_due_follows(noon).await.unwrap());
}

#[tokio::test]
async fn requested_albums_are_the_members_own_whatever_became_of_them() {
    let f = fixture().await;
    let (artist, album) = artist(&f.db).await;
    let rejected = scalar(
        &f.db,
        format!(
            "INSERT INTO request (member_id, catalog_item_id, category, state, created_at,
                 updated_at)
             VALUES ({}, {artist}, 'music', 'rejected', ?1, ?1) RETURNING id",
            f.mia
        ),
    )
    .await;
    scalar(
        &f.db,
        format!(
            "INSERT INTO request_item (request_id, member_id, catalog_item_id, decision)
             VALUES ({rejected}, {}, {album}, 'rejected') RETURNING ?1 IS NOT NULL",
            f.mia
        ),
    )
    .await;
    let release_group = Uuid::parse_str("b1392450-e666-3926-a536-22c65f834433").unwrap();
    assert_eq!(
        f.db.requested_albums(f.mia).await.unwrap(),
        BTreeSet::from([release_group])
    );
    assert_eq!(
        f.db.requested_albums(f.noah).await.unwrap(),
        BTreeSet::new()
    );
    // Seasons are no albums.
    f.request(f.noah, 1, RequestFollow::No).await.unwrap();
    assert_eq!(
        f.db.requested_albums(f.noah).await.unwrap(),
        BTreeSet::new()
    );
}

/// A follow of a kind a newer release might follow (here: a movie) is
/// left out instead of failing the lists (ADR 0012).
#[tokio::test]
async fn follows_of_unknown_kinds_are_left_out_of_lists() {
    let f = fixture().await;
    f.db.follow(f.mia, f.series, Category::Series, Cutoff::Seasons(2), t0())
        .await
        .unwrap();
    let movie = scalar(
        &f.db,
        "INSERT INTO catalog_item (kind, title, created_at, updated_at)
         VALUES ('movie', 'Something', ?1, ?1) RETURNING id"
            .to_owned(),
    )
    .await;
    scalar(
        &f.db,
        format!(
            "INSERT INTO follow (member_id, catalog_item_id, category, known_through, created_at)
             VALUES ({}, {movie}, 'movies', '2026-11-02', ?1) RETURNING id",
            f.mia
        ),
    )
    .await;
    let mine = f.mine(f.mia).await;
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].catalog_item_id, f.series);
    assert_eq!(f.db.due_follows(t0()).await.unwrap().len(), 1);
    // Checked: the movie follow alone gives the job no work.
    let series = &f.mine(f.mia).await[0];
    f.db.follow_checked(series.id, f.mia, None, t0() + TimeDelta::hours(1))
        .await
        .unwrap();
    assert!(!f.db.has_due_follows(t0()).await.unwrap());
}

#[tokio::test]
async fn a_request_with_follow_starts_the_follow_in_the_same_transaction() {
    let f = fixture().await;
    let created = f
        .request(
            f.mia,
            1,
            RequestFollow::Start {
                cutoff: Cutoff::Seasons(2),
            },
        )
        .await
        .unwrap();
    let follows = f.mine(f.mia).await;
    assert_eq!(follows.len(), 1);
    assert_eq!(follows[0].cutoff, Cutoff::Seasons(2));
    assert!(
        f.db.get_request(created.request_id)
            .await
            .unwrap()
            .unwrap()
            .follow
    );

    // A refused request starts nothing.
    let refused = f
        .request(
            f.noah,
            1,
            RequestFollow::Start {
                cutoff: Cutoff::Seasons(2),
            },
        )
        .await;
    assert!(refused.is_ok());
    let again = f
        .request(
            f.noah,
            1,
            RequestFollow::Start {
                cutoff: Cutoff::Seasons(3),
            },
        )
        .await;
    assert!(matches!(again, Err(DbError::RequestOpen)), "{again:?}");
    assert_eq!(f.mine(f.noah).await[0].cutoff, Cutoff::Seasons(2));

    // Without follow, nothing is followed.
    f.db.unfollow(f.noah, f.mine(f.noah).await[0].id, t0())
        .await
        .unwrap();
    let plain = f.request(f.noah, 2, RequestFollow::No).await.unwrap();
    assert_eq!(f.mine(f.noah).await.len(), 0);
    assert!(
        !f.db
            .get_request(plain.request_id)
            .await
            .unwrap()
            .unwrap()
            .follow
    );
}

#[tokio::test]
async fn a_found_season_moves_the_cutoff_with_its_request() {
    let f = fixture().await;
    let (follow, _) =
        f.db.follow(f.mia, f.series, Category::Series, Cutoff::Seasons(2), t0())
            .await
            .unwrap();
    let later = t0() + TimeDelta::days(1);
    f.request(
        f.mia,
        3,
        RequestFollow::Found {
            follow_id: follow.id,
            cutoff: Cutoff::Seasons(3),
        },
    )
    .await
    .unwrap();
    let after = &f.mine(f.mia).await[0];
    assert_eq!(after.cutoff, Cutoff::Seasons(3));
    assert!(after.checked_at.is_some());

    // Another member's follow id does not move anything.
    let wrong = f
        .request(
            f.noah,
            3,
            RequestFollow::Found {
                follow_id: follow.id,
                cutoff: Cutoff::Seasons(9),
            },
        )
        .await;
    assert!(matches!(wrong, Err(DbError::FollowEnded)), "{wrong:?}");
    let (open,): (i64,) = sqlx::query_as("SELECT count(*) FROM request WHERE member_id = ?")
        .bind(f.noah)
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert_eq!(open, 0, "the refused request was rolled back");

    // Checked without a request: the cutoff only moves up.
    assert!(
        f.db.follow_checked(follow.id, f.mia, Some(1), later)
            .await
            .unwrap()
    );
    let after = &f.mine(f.mia).await[0];
    assert_eq!(after.cutoff, Cutoff::Seasons(3));
    assert_eq!(after.checked_at, Some(later));

    // Ended: neither moves it.
    f.db.unfollow(f.mia, follow.id, later).await.unwrap();
    assert!(
        !f.db
            .follow_checked(follow.id, f.mia, Some(4), later)
            .await
            .unwrap()
    );
    assert!(
        !f.db
            .has_due_follows(later + TimeDelta::days(9))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn requests_made_with_follow_before_the_migration_get_their_follow() {
    let follows_version = 20_261_108_170_000;
    let before = Migrator::with_migrations(
        EMBEDDED
            .iter()
            .filter(|m| m.version < follows_version)
            .cloned()
            .collect(),
    );
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .in_memory(true)
        .foreign_keys(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    before.run(&pool).await.unwrap();
    let db = Database { pool };
    let mia = member(&db, "mia").await;
    let noah = member(&db, "noah").await;
    let lea = member(&db, "lea").await;
    let (series, seasons) = series(&db).await;
    for (member, follow, season, state) in [
        (mia, 1, 1, "pending_approval"),
        (mia, 1, 3, "approved"),
        (noah, 0, 1, "pending_approval"),
        // Withdrawn or rejected: the follow went with the request.
        (lea, 1, 1, "cancelled"),
        (lea, 1, 2, "rejected"),
    ] {
        let request = scalar(
            &db,
            format!(
                "INSERT INTO request (member_id, catalog_item_id, category, state, follow,
                     created_at, updated_at)
                 VALUES ({member}, {series}, 'series', '{state}', {follow}, ?1, ?1)
                 RETURNING id"
            ),
        )
        .await;
        scalar(
            &db,
            format!(
                "INSERT INTO request_item (request_id, member_id, catalog_item_id,
                     season_number, decision)
                 VALUES ({request}, {member}, {}, {season}, '{state}') RETURNING ?1 IS NOT NULL",
                seasons[season - 1]
            ),
        )
        .await;
    }

    migrator().run(db.pool()).await.unwrap();

    let follows = db.due_follows(t0()).await.unwrap();
    assert_eq!(follows.len(), 1, "only Mia asked with follow and kept it");
    let follow = &follows[0];
    assert_eq!((follow.member_id, follow.catalog_item_id), (mia, series));
    // Season 2 is the last with an air date, but Mia asked for season 3.
    assert_eq!(follow.cutoff, Cutoff::Seasons(3));
    assert_eq!(follow.category, Category::Series);
    assert_eq!(follow.created_at, t0());
}
const PERFORMER: &str = "9e7f0000-0000-4000-8000-0000000000aa";

/// A performer (placeholder name) and two scenes of hers in `db`.
async fn performer(db: &Database) -> (i64, [i64; 2]) {
    let performer = scalar(
        db,
        "INSERT INTO catalog_item (kind, title, created_at, updated_at)
         VALUES ('performer', 'Performer P', ?1, ?1) RETURNING id"
            .to_owned(),
    )
    .await;
    let mut scenes = [0; 2];
    for (i, scene) in scenes.iter_mut().enumerate() {
        *scene = scalar(
            db,
            format!(
                "INSERT INTO catalog_item (kind, title, created_at, updated_at)
                 VALUES ('scene', 'Scene {i}', ?1, ?1) RETURNING id"
            ),
        )
        .await;
        sqlx::query(
            "INSERT INTO catalog_key (catalog_item_id, scheme, value) VALUES (?, 'stashdb_scene', ?)",
        )
        .bind(*scene)
        .bind(format!("5ce0e000-0000-4000-8000-00000000000{i}"))
        .execute(db.pool())
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO catalog_key (catalog_item_id, scheme, value) VALUES (?, 'stashdb_performer', ?)",
    )
    .bind(performer)
    .bind(PERFORMER)
    .execute(db.pool())
    .await
    .unwrap();
    (performer, scenes)
}

impl Fixture {
    fn adult_source(&self) -> SourceTarget {
        SourceTarget {
            category: Category::Adult,
            service_instance_id: self.sonarr,
            quality_profile_id: 1,
            root_folder_path: "/private".into(),
            minimum_availability: None,
            metadata_profile_id: None,
        }
    }

    async fn request_scene(
        &self,
        member: i64,
        scene: i64,
        follow: RequestFollow,
        at: DateTime<Utc>,
    ) -> crate::requests::Created {
        let source = self.adult_source();
        self.db
            .create_request(
                &NewRequest {
                    member_id: member,
                    catalog_item_id: scene,
                    follow,
                    units: &[Unit {
                        catalog_item_id: scene,
                        season_number: None,
                    }],
                    mode: RequestMode::Approval,
                    source: &source,
                },
                SYSTEM,
                at,
            )
            .await
            .unwrap()
    }

    async fn capped(&self, member: i64) -> Vec<crate::notifications::NotificationRecord> {
        self.db
            .list_notifications(crate::notifications::Stream::Member(member), None, 50)
            .await
            .unwrap()
            .into_iter()
            .filter(|n| n.kind == starr_core::notifications::NotificationKind::FollowCapped)
            .collect()
    }
}

#[tokio::test]
async fn performer_follows_count_their_own_requests_for_the_weekly_cap() {
    let f = fixture().await;
    let (performer, scenes) = performer(&f.db).await;
    let (mia, created) =
        f.db.follow(
            f.mia,
            performer,
            Category::Adult,
            Cutoff::Date(day(2026, 11, 1)),
            t0(),
        )
        .await
        .unwrap();
    assert!(created);
    assert_eq!(mia.kind, ItemKind::Performer);
    assert_eq!(mia.stash_id, Some(Uuid::parse_str(PERFORMER).unwrap()));
    assert_eq!(mia.cutoff, Cutoff::Date(day(2026, 11, 1)));
    assert_eq!(mia.capped_scenes, None);
    assert_eq!(f.db.due_follows(t0()).await.unwrap().len(), 1);
    assert!(f.db.has_due_follows(t0()).await.unwrap());

    // The follow requests one scene; Mia asks for the other by hand.
    let found = f
        .request_scene(
            f.mia,
            scenes[0],
            RequestFollow::Found {
                follow_id: mia.id,
                cutoff: mia.cutoff,
            },
            t0(),
        )
        .await;
    f.request_scene(f.mia, scenes[1], RequestFollow::No, t0())
        .await;
    let (follow_id,): (Option<i64>,) = sqlx::query_as("SELECT follow_id FROM request WHERE id = ?")
        .bind(found.request_id)
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert_eq!(follow_id, Some(mia.id));
    // Only the follow's own request counts, and only within the window.
    let week_ago = t0() - starr_core::follows::CAP_WINDOW;
    assert_eq!(
        f.db.follow_requests_since(mia.id, week_ago).await.unwrap(),
        1
    );
    assert_eq!(f.db.follow_requests_since(mia.id, t0()).await.unwrap(), 0);
    // Both scenes are Mia's from now on, Noah has none.
    assert_eq!(f.db.requested_scenes(f.mia).await.unwrap().len(), 2);
    assert!(f.db.requested_scenes(f.noah).await.unwrap().is_empty());
    // The request event names neither performer nor scene.
    let (detail,): (Option<String>,) =
        sqlx::query_as("SELECT detail FROM request_event WHERE request_id = ? ORDER BY id LIMIT 1")
            .bind(found.request_id)
            .fetch_one(f.db.pool())
            .await
            .unwrap();
    assert_eq!(
        detail.as_deref(),
        Some(crate::requests::FOUND_BY_PRIVATE_FOLLOW)
    );

    // Four over the cap: the hint, and one notification per week.
    assert!(f.db.note_follow_cap(&mia, 4, 0, t0()).await.unwrap());
    assert!(f.db.note_follow_cap(&mia, 3, 0, t0()).await.unwrap());
    assert_eq!(f.mine(f.mia).await[0].capped_scenes, Some(3));
    let notes = f.capped(f.mia).await;
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].payload.title, "Performer P");
    assert_eq!(notes[0].payload.category, "adult");
    assert_eq!(f.capped(f.noah).await, Vec::new());
    // Next week again; nothing over clears the hint without notifying.
    assert!(f.db.note_follow_cap(&mia, 2, 1, t0()).await.unwrap());
    assert_eq!(f.capped(f.mia).await.len(), 2);
    assert!(f.db.note_follow_cap(&mia, 0, 1, t0()).await.unwrap());
    assert_eq!(f.mine(f.mia).await[0].capped_scenes, None);
    assert_eq!(f.capped(f.mia).await.len(), 2);
    // Ended: nothing is noted any more.
    assert!(f.db.unfollow(f.mia, mia.id, t0()).await.unwrap());
    assert!(!f.db.note_follow_cap(&mia, 5, 2, t0()).await.unwrap());
    assert_eq!(f.capped(f.mia).await.len(), 2);
}
