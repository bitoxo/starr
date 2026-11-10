//! Privacy gate test environment (spec 0007 slice 1): the movies and
//! series setup plus the adult category switched on with the operator
//! statement, two members (Ada entitled to adult, Ben not), a second admin
//! (Ops), adult rows seeded straight into the database (there is no adult
//! source yet) and a `StashDB` fake as the adult catalog (slice 2), with an
//! image proxy whose cache lives in a temporary directory. Every adult text
//! is a neutral placeholder; the sweep looks for them in every answer.
#![allow(clippy::unwrap_used, clippy::too_many_lines, dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use starr_adapter_musicbrainz::covers::CoverArchive;
use starr_adapter_tmdb::images::ImageCdn;
use starr_server::{ImageProxy, app};
use starr_worker::image_cache::ImageCache;
use url::Url;
use wiremock::MockServer;

use super::media::{Member, Setup, member, put, setup};
use super::services::patch;
use super::stashdb::{IMAGE, PERFORMER_P, SCENE_A, STUDIO_S, connect_stashdb, fake_stashdb};
use super::{Req, TestResponse, elevate_in_place, get, post};

/// Texts that name the seeded adult content; none may leave the gate.
pub const MARKERS: [&str; 9] = [
    "Scene A",
    // The scene's description (spec 0008 Q14), treated like a title.
    "Details D",
    "Performer P",
    "Studio S",
    "scene-a-poster",
    "Reason R",
    "Message M",
    // The followed performer's and her studio's `StashDB` UUIDs.
    PERFORMER_P,
    STUDIO_S,
];

pub const OPS_PASSWORD: &str = "ops password long enough";

/// The image proxy's cache directory, removed with the test.
pub struct CacheDir(pub PathBuf);

impl CacheDir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "starr-privacy-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }

    /// Every file below the directory.
    pub fn files(&self) -> Vec<PathBuf> {
        fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.0, &mut out);
        out
    }
}

impl Drop for CacheDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub struct Privacy {
    pub setup: Setup,
    /// Entitled to adult (and movies).
    pub ada: Member,
    /// Movies only; holds adult rows as after a removed entitlement.
    pub ben: Member,
    /// A second admin.
    pub ops: String,
    pub ada_request: i64,
    pub ben_request: i64,
    pub acquisition: i64,
    pub ada_follow: i64,
    /// The adult catalog's fake.
    pub stashdb: MockServer,
    pub stashdb_id: i64,
    /// The image proxy's cache directory.
    pub images: CacheDir,
}

impl Privacy {
    pub fn pool(&self) -> &SqlitePool {
        self.setup.env.app.state.db.pool()
    }

    pub async fn admin(&self, req: Req) -> TestResponse {
        self.setup.admin(req).await
    }

    pub async fn ops(&self, req: Req) -> TestResponse {
        req.cookie(&self.ops).send(&self.setup.env.app).await
    }

    pub async fn as_member(&self, member: &Member, req: Req) -> TestResponse {
        self.setup.as_member(member, req).await
    }

    /// Unlock (or lock) the private category in a session.
    pub async fn set_unlocked(&self, cookie: &str, unlocked: bool) -> TestResponse {
        put("/api/v1/auth/private", json!({ "unlocked": unlocked }))
            .cookie(cookie)
            .send(&self.setup.env.app)
            .await
    }

    pub async fn switch_adult(&self, enabled: bool) -> TestResponse {
        self.admin(patch(
            "/api/v1/categories/adult",
            json!({ "enabled": enabled, "operatorStatement": enabled }),
        ))
        .await
    }
}

/// Assert that `res` names none of the seeded adult texts.
pub fn assert_clean(what: &str, res: &TestResponse) {
    for marker in MARKERS {
        assert!(
            !res.raw.contains(marker),
            "{what} names {marker:?}: {}",
            res.raw
        );
    }
}

/// Whether `res` names the seeded adult title.
pub fn names_adult(res: &TestResponse) -> bool {
    res.raw.contains("Scene A")
}

async fn exec(pool: &SqlitePool, sql: &str) -> i64 {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
}

const AT: &str = "2026-11-01T11:00:00.000Z";

pub async fn privacy() -> Privacy {
    let mut setup = setup().await;
    // An image proxy whose upstreams are dead ends: only the adult source
    // is ever reached (through the StashDB fake).
    let images = CacheDir::new();
    let dead = Url::parse("http://127.0.0.1:9/").unwrap();
    let proxy = ImageProxy::new(
        ImageCdn::with_base(dead.clone()).unwrap(),
        CoverArchive::with_base(dead).unwrap(),
        ImageCache::new(images.0.clone()),
    );
    setup.env.app.state = setup.env.app.state.clone().with_images(proxy);
    setup.env.app.router = app(setup.env.app.state.clone());
    let res = setup
        .admin(patch(
            "/api/v1/categories/adult",
            json!({ "enabled": true, "operatorStatement": true }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let ada = member(
        &setup.env,
        "ada",
        json!([
            { "category": "movies", "enabled": true, "requestMode": "approval" },
            { "category": "adult", "enabled": true, "requestMode": "approval" },
        ]),
    )
    .await;
    let ben = member(
        &setup.env,
        "ben",
        json!([{ "category": "movies", "enabled": true, "requestMode": "approval" }]),
    )
    .await;
    elevate_in_place(&setup.env.app, &setup.env.admin).await;
    let res = setup
        .admin(post("/api/v1/admins", json!({ "username": "ops" })))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let token = res.body["setPasswordLink"]["url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    let res = post(
        &format!("/api/v1/password-tokens/{token}"),
        json!({ "password": OPS_PASSWORD }),
    )
    .send(&setup.env.app)
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let ops = res.session_cookie().unwrap();

    let stashdb = fake_stashdb("probe_read.json").await;
    let stashdb_id = connect_stashdb(&setup.env, &stashdb).await;

    let pool = setup.env.app.state.db.pool().clone();
    let radarr = setup.radarr;
    // A private title (adult full-length movies reuse the `movie` kind,
    // ADR 0012), its failed acquisition at the Radarr fake, Ada's request
    // with its history, Ben's pending request (as after his entitlement
    // was removed) and Ada's follow.
    let item = exec(
        &pool,
        &format!(
            "INSERT INTO catalog_item (kind, title, year, poster_path, created_at, updated_at)
             VALUES ('movie', 'Scene A with Performer P', 1987, '/scene-a-poster.jpg', '{AT}', '{AT}')"
        ),
    )
    .await;
    let acquisition = exec(
        &pool,
        &format!(
            "INSERT INTO acquisition (catalog_item_id, service_instance_id, category, state,
                 quality_profile_id, root_folder_path, last_error, source_message, error_code,
                 created_at, updated_at)
             VALUES ({item}, {radarr}, 'adult', 'failed', 4, '/adult/', 'Studio S refused',
                 'Message M', 'rejected', '{AT}', '{AT}')"
        ),
    )
    .await;
    let ada_request = exec(
        &pool,
        &format!(
            "INSERT INTO request (member_id, catalog_item_id, category, state, revoke_reason,
                 created_at, updated_at)
             VALUES ({}, {item}, 'adult', 'failed', NULL, '{AT}', '{AT}')",
            ada.id
        ),
    )
    .await;
    exec(
        &pool,
        &format!(
            "INSERT INTO request_item (request_id, member_id, catalog_item_id, decision,
                 acquisition_id, decided_at)
             VALUES ({ada_request}, {}, {item}, 'approved', {acquisition}, '{AT}')",
            ada.id
        ),
    )
    .await;
    exec(
        &pool,
        &format!(
            "INSERT INTO request_event (request_id, at, from_state, to_state, trigger, actor, detail)
             VALUES ({ada_request}, '{AT}', 'pending_approval', 'approved', 'admin', 'root',
                 'Reason R')"
        ),
    )
    .await;
    let ben_request = exec(
        &pool,
        &format!(
            "INSERT INTO request (member_id, catalog_item_id, category, state, created_at,
                 updated_at)
             VALUES ({}, {item}, 'adult', 'pending_approval', '{AT}', '{AT}')",
            ben.id
        ),
    )
    .await;
    exec(
        &pool,
        &format!(
            "INSERT INTO request_item (request_id, member_id, catalog_item_id, decision)
             VALUES ({ben_request}, {}, {item}, 'pending_approval')",
            ben.id
        ),
    )
    .await;
    // Ada follows Performer P (spec 0007 slice 5), with scenes beyond the
    // weekly cap noted on the follow.
    let followed = exec(
        &pool,
        &format!(
            "INSERT INTO catalog_item (kind, title, poster_path, created_at, updated_at)
             VALUES ('performer', 'Performer P', 'stashdb:{IMAGE}', '{AT}', '{AT}')"
        ),
    )
    .await;
    exec(
        &pool,
        &format!(
            "INSERT INTO catalog_key (catalog_item_id, scheme, value)
             VALUES ({followed}, 'stashdb_performer', '{PERFORMER_P}')"
        ),
    )
    .await;
    let ada_follow = exec(
        &pool,
        &format!(
            "INSERT INTO follow (member_id, catalog_item_id, category, known_through,
                 capped_scenes, checked_at, created_at)
             VALUES ({}, {followed}, 'adult', '2026-11-01', 4, '{AT}', '{AT}')",
            ada.id
        ),
    )
    .await;
    // In-app notifications: Ada's and Ben's, and the admin stream's.
    let payload = |member: &str| {
        json!({
            "title": "Scene A with Performer P",
            "year": 1987,
            "category": "adult",
            "requestId": ada_request,
            "acquisitionId": acquisition,
            "reason": "Reason R",
            "member": member,
        })
        .to_string()
    };
    for (member, kind, who) in [
        (Some(ada.id), "request_approved", ""),
        (Some(ben.id), "request_rejected", ""),
        (None, "approval_pending", "ada"),
        (None, "acquisition_failed", "ada"),
    ] {
        let mut payload: Value = serde_json::from_str(&payload(who)).unwrap();
        if who.is_empty() {
            payload.as_object_mut().unwrap().remove("member");
        }
        sqlx::query(
            "INSERT INTO notification (member_id, kind, payload, dedup_key, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(member)
        .bind(kind)
        .bind(payload.to_string())
        .bind(format!("privacy-{kind}"))
        .bind(AT)
        .execute(&pool)
        .await
        .unwrap();
    }
    Privacy {
        setup,
        ada,
        ben,
        ops,
        ada_request,
        ben_request,
        acquisition,
        ada_follow,
        stashdb,
        stashdb_id,
        images,
    }
}

/// Every documented `GET` path with its parameters filled in so they hit
/// the seeded adult rows where they can.
pub fn documented_gets(p: &Privacy) -> Vec<String> {
    let doc: Value = serde_json::from_str(&starr_server::openapi_json()).unwrap();
    let mut out = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        if item.get("get").is_none() {
            continue;
        }
        // Several fillings of `{id}`: the requests, the acquisition, the
        // follow, a member, a service.
        let ids = [
            p.ada_request,
            p.ben_request,
            p.acquisition,
            p.ada_follow,
            p.ada.id,
            p.ben.id,
            p.setup.radarr,
            p.stashdb_id,
        ];
        for id in ids {
            let concrete: Vec<String> = path
                .split('/')
                .map(|seg| match seg {
                    "{id}" | "{serviceId}" => id.to_string(),
                    "{category}" => "adult".to_owned(),
                    "{kind}" => "movie".to_owned(),
                    "{source}" => "stashdb".to_owned(),
                    "{size}" => "300".to_owned(),
                    "{ref}" => IMAGE.to_owned(),
                    "{sceneId}" => SCENE_A.to_owned(),
                    "{performerId}" => PERFORMER_P.to_owned(),
                    "{studioId}" => STUDIO_S.to_owned(),
                    "{list}" => "trending".to_owned(),
                    s if s.starts_with('{') => "1".to_owned(),
                    s => s.to_owned(),
                })
                .collect();
            let mut concrete = concrete.join("/");
            // Searches look for the seeded title.
            if concrete.ends_with("/search") {
                concrete.push_str("?query=Scene");
            }
            out.push(concrete);
            if !path.contains('{') {
                break;
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// GET every documented path as `cookie` and assert none names adult;
/// then the reading `POST`s (the private search, which takes its text in
/// the body).
pub async fn sweep(p: &Privacy, who: &str, cookie: &str) -> usize {
    let mut checked = 0;
    for path in documented_gets(p) {
        let res = get(&path).cookie(cookie).send(&p.setup.env.app).await;
        assert_clean(&format!("{who}: GET {path}"), &res);
        checked += 1;
    }
    let res = post("/api/v1/private/search", json!({ "query": "Scene" }))
        .cookie(cookie)
        .send(&p.setup.env.app)
        .await;
    assert_clean(&format!("{who}: POST /api/v1/private/search"), &res);
    // Following through a closed gate does not exist either (slice 5).
    let res = post(
        "/api/v1/private/follows",
        json!({ "kind": "studio", "id": STUDIO_S }),
    )
    .cookie(cookie)
    .send(&p.setup.env.app)
    .await;
    assert!(!res.status.is_success(), "{who}: {}", res.raw);
    assert_clean(&format!("{who}: POST /api/v1/private/follows"), &res);
    // Neither do the search narrowed to a studio (spec 0008 Q7 C) nor the
    // undo of an unfollow (PRIV-17).
    let res = post(
        "/api/v1/private/search",
        json!({ "query": "Scene", "studio": STUDIO_S }),
    )
    .cookie(cookie)
    .send(&p.setup.env.app)
    .await;
    assert!(!res.status.is_success(), "{who}: {}", res.raw);
    assert_clean(
        &format!("{who}: POST /api/v1/private/search (studio)"),
        &res,
    );
    let restore = format!("/api/v1/private/follows/{}/restore", p.ada_follow);
    let res = post(&restore, json!({}))
        .cookie(cookie)
        .send(&p.setup.env.app)
        .await;
    assert!(!res.status.is_success(), "{who}: {}", res.raw);
    assert_clean(&format!("{who}: POST {restore}"), &res);
    checked + 4
}
