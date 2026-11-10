//! The privacy gate of the adult category (spec 0007 slice 1, ADR 0014):
//! every leak point of the spec's table as a test, with adult rows seeded
//! into the database, two members (Ada entitled, Ben not) and two admins
//! (Root locked, Ops unlocked).
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::{Method, StatusCode, header};
use chrono::TimeDelta;
use common::media::put;
use common::privacy::{
    MARKERS, Privacy, assert_clean, documented_gets, names_adult, privacy, sweep,
};
use common::services::patch;
use common::stashdb::{IMAGE, IMAGE_BYTES, SCENE_A, graphql_requests, image_requests};
use common::{Req, get, login, post};
use serde_json::{Value, json};
use starr_worker::image_cache::{ImageCache, ImageFormat};

/// Documented `GET` endpoints at the time of writing: the sweep must reach
/// at least this many paths, so a broken document cannot pass vacuously.
const MIN_SWEPT: usize = 60;

async fn me(p: &Privacy, cookie: &str) -> Value {
    get("/api/v1/auth/me")
        .cookie(cookie)
        .send(&p.setup.env.app)
        .await
        .body
}

#[tokio::test]
async fn nothing_adult_leaves_a_closed_gate() {
    let p = privacy().await;
    let root = p.setup.env.admin.clone();

    // Ben has no entitlement, Ada has not unlocked, Root has not unlocked.
    let asked = graphql_requests(&p.stashdb).await;
    assert!(sweep(&p, "ben", &p.ben.cookie).await >= MIN_SWEPT);
    sweep(&p, "ada (locked)", &p.ada.cookie).await;
    sweep(&p, "root (locked)", &root).await;
    // Unlocking does not help without the entitlement.
    assert_eq!(
        p.set_unlocked(&p.ben.cookie, true).await.status,
        StatusCode::NOT_FOUND
    );
    sweep(&p, "ben (tried to unlock)", &p.ben.cookie).await;
    // A closed gate never even asks the adult catalog or its images.
    assert_eq!(graphql_requests(&p.stashdb).await, asked);
    assert_eq!(image_requests(&p.stashdb).await, 0);
    assert!(p.images.files().is_empty(), "{:?}", p.images.files());

    // Open: Ada sees her request and notification, Ops the admin rows.
    assert_eq!(
        p.set_unlocked(&p.ada.cookie, true).await.status,
        StatusCode::OK
    );
    assert_eq!(p.set_unlocked(&p.ops, true).await.status, StatusCode::OK);
    // Her private requests are in the private area's own tab, never in the
    // general list (spec 0007 story 7).
    let mine = p.as_member(&p.ada, get("/api/v1/private/requests")).await;
    assert!(names_adult(&mine), "{}", mine.raw);
    let general = p.as_member(&p.ada, get("/api/v1/requests")).await;
    assert_clean("ada (unlocked): general requests", &general);
    let notes = p.as_member(&p.ada, get("/api/v1/notifications")).await;
    assert!(names_adult(&notes), "{}", notes.raw);
    let admin = p.ops(get("/api/v1/admin/requests")).await;
    assert!(names_adult(&admin), "{}", admin.raw);
    // ...but never outside the private area: title cards, follows' series
    // views and Discover stay clean (Q11).
    let discover = p.as_member(&p.ada, get("/api/v1/discover")).await;
    assert_clean("ada (unlocked): discover", &discover);
    // The private area itself shows the catalog.
    let scene = p
        .as_member(&p.ada, get(&format!("/api/v1/private/scenes/{SCENE_A}")))
        .await;
    assert_eq!(scene.status, StatusCode::OK, "{}", scene.raw);
    assert!(names_adult(&scene), "{}", scene.raw);

    // Switched off, the gate closes for everyone at once.
    assert_eq!(p.switch_adult(false).await.status, StatusCode::OK);
    sweep(&p, "ada (unlocked, category off)", &p.ada.cookie).await;
    sweep(&p, "ops (unlocked, category off)", &p.ops).await;
}

#[tokio::test]
async fn members_behind_the_gate_get_no_private_notification_and_no_count() {
    let p = privacy().await;
    // Ada (entitled, locked): neither listed nor counted (spec 0008 Q24 A),
    // so the badge and the list agree.
    let page = p.as_member(&p.ada, get("/api/v1/notifications")).await;
    assert_clean("ada notifications", &page);
    assert_eq!(page.body["items"], json!([]), "{}", page.raw);
    assert_eq!(page.body["unread"], 0, "locked: not counted");
    // "Mark all read" while locked leaves the hidden ones unread.
    let marked = p
        .as_member(&p.ada, post("/api/v1/notifications/read", json!({})))
        .await;
    assert_eq!(marked.body["unread"], 0);

    // Ben (not entitled): nothing at all.
    let page = p.as_member(&p.ben, get("/api/v1/notifications")).await;
    assert_eq!(page.body["items"], json!([]), "{}", page.raw);
    assert_eq!(page.body["unread"], 0);
    let mine = p
        .as_member(&p.ben, get("/api/v1/requests?hidden=true"))
        .await;
    assert_eq!(mine.body, json!([]), "{}", mine.raw);
    let follows = p.as_member(&p.ada, get("/api/v1/follows")).await;
    assert_eq!(follows.body, json!([]), "{}", follows.raw);

    // Ada unlocked: the title and the count, still unread.
    p.set_unlocked(&p.ada.cookie, true).await;
    let page = p.as_member(&p.ada, get("/api/v1/notifications")).await;
    let items = page.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{}", page.raw);
    assert_eq!(items[0]["redacted"], false);
    assert_eq!(items[0]["read"], false);
    assert_eq!(items[0]["title"], "Scene A with Performer P");
    assert_eq!(page.body["unread"], 1);
    // Her performer follow is in the private area's tab only (slice 5).
    let follows = p.as_member(&p.ada, get("/api/v1/follows")).await;
    assert_eq!(follows.body, json!([]), "{}", follows.raw);
    let follows = p.as_member(&p.ada, get("/api/v1/private/follows")).await;
    assert_eq!(follows.body.as_array().unwrap().len(), 1, "{}", follows.raw);
    assert_eq!(follows.body[0]["name"], "Performer P");
    let mine = p.as_member(&p.ada, get("/api/v1/private/requests")).await;
    assert_eq!(mine.body[0]["id"], p.ada_request);
}

#[tokio::test]
async fn actions_on_hidden_rows_answer_not_found() {
    let p = privacy().await;
    let ben = p.ben_request;
    let ada = p.ada_request;
    for (member, id) in [(&p.ben, ben), (&p.ada, ada)] {
        for req in [
            post(&format!("/api/v1/requests/{id}/cancel"), json!({})),
            post(&format!("/api/v1/requests/{id}/withdraw"), json!({})),
            put(
                &format!("/api/v1/requests/{id}/hidden"),
                json!({ "hidden": true }),
            ),
        ] {
            let res = p.as_member(member, req).await;
            assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
            assert_eq!(res.code(), "requests.not_found");
        }
    }
    let res = p
        .as_member(
            &p.ada,
            Req::new(
                Method::DELETE,
                &format!("/api/v1/follows/{}", p.ada_follow),
                None,
            ),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
    assert_eq!(res.code(), "follows.not_found");
    // Nothing changed meanwhile.
    let (state,): (String,) = sqlx::query_as("SELECT state FROM request WHERE id = ?")
        .bind(ben)
        .fetch_one(p.pool())
        .await
        .unwrap();
    assert_eq!(state, "pending_approval");

    // Unlocked, Ada reaches her own rows again.
    p.set_unlocked(&p.ada.cookie, true).await;
    let res = p
        .as_member(
            &p.ada,
            put(
                &format!("/api/v1/requests/{ada}/hidden"),
                json!({ "hidden": true }),
            ),
        )
        .await;
    // (A failed request is not finished: refused for that reason, not hidden.)
    assert_eq!(res.code(), "requests.not_finished", "{}", res.raw);
}

#[tokio::test]
async fn unlock_is_per_session_closes_after_thirty_idle_minutes_and_on_logout() {
    let p = privacy().await;
    let clock = &p.setup.env.app.clock;
    // Only accounts that may unlock see the switch.
    assert_eq!(me(&p, &p.ben.cookie).await["privateCategory"], Value::Null);
    let ada = me(&p, &p.ada.cookie).await;
    assert_eq!(ada["privateCategory"]["unlocked"], false, "{ada}");
    let root = me(&p, &p.setup.env.admin).await;
    assert_eq!(root["privateCategory"]["unlocked"], false, "{root}");
    let res = p.set_unlocked(&p.ben.cookie, true).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.code(), "private.not_found");

    let res = p.set_unlocked(&p.ada.cookie, true).await;
    assert_eq!(res.body["unlocked"], true, "{}", res.raw);
    // Ada's other session (another device) stays locked.
    let other = login(&p.setup.env.app, "ada", "a long enough password")
        .await
        .session_cookie()
        .unwrap();
    assert_eq!(me(&p, &other).await["privateCategory"]["unlocked"], false);

    // Image requests in the private area count as activity...
    clock.advance(TimeDelta::minutes(20));
    let image = format!("/api/v1/images/stashdb/300/{IMAGE}");
    let res = p.as_member(&p.ada, get(&image)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    clock.advance(TimeDelta::minutes(20));
    assert_eq!(
        me(&p, &p.ada.cookie).await["privateCategory"]["unlocked"],
        true
    );
    // Listing her private requests is polling, not activity (the tab
    // refreshes itself): it does not keep the unlock open.
    let mine = p.as_member(&p.ada, get("/api/v1/private/requests")).await;
    assert!(names_adult(&mine));
    // ...polling elsewhere is not: 30 idle minutes close it.
    clock.advance(TimeDelta::minutes(11));
    assert_eq!(
        me(&p, &p.ada.cookie).await["privateCategory"]["unlocked"],
        false
    );
    let mine = p.as_member(&p.ada, get("/api/v1/private/requests")).await;
    assert_eq!(mine.status, StatusCode::NOT_FOUND, "{}", mine.raw);
    assert_clean("ada after 30 idle minutes", &mine);

    // Locking by hand and logout close it too.
    p.set_unlocked(&p.ada.cookie, true).await;
    let res = p.set_unlocked(&p.ada.cookie, false).await;
    assert_eq!(res.body["unlocked"], false);
    p.set_unlocked(&p.ada.cookie, true).await;
    post("/api/v1/auth/logout", json!({}))
        .cookie(&p.ada.cookie)
        .send(&p.setup.env.app)
        .await;
    let again = login(&p.setup.env.app, "ada", "a long enough password")
        .await
        .session_cookie()
        .unwrap();
    assert_eq!(me(&p, &again).await["privateCategory"]["unlocked"], false);

    // Off: nobody may unlock.
    p.switch_adult(false).await;
    assert_eq!(me(&p, &again).await["privateCategory"], Value::Null);
    assert_eq!(
        p.set_unlocked(&p.ops, true).await.status,
        StatusCode::NOT_FOUND
    );
}

/// The kinds a notification preferences answer offers.
fn offered(res: &common::TestResponse) -> Vec<String> {
    res.body["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn notification_preferences_offer_only_kinds_of_the_members_categories_and_gate() {
    // PRV-01 (spec 0008 F2): the server filters, so the profile cannot name
    // the private category's kinds to a member who may not see them.
    let p = privacy().await;
    let path = "/api/v1/me/notification-preferences";
    let set = |kinds: Value| put(path, json!({ "email": kinds }));

    // Ben (movies only): request kinds, nothing about follows or the cap.
    let ben = p.as_member(&p.ben, get(path)).await;
    assert_eq!(ben.status, StatusCode::OK, "{}", ben.raw);
    let kinds = offered(&ben);
    assert!(kinds.contains(&"request_available".to_owned()), "{kinds:?}");
    assert!(!kinds.contains(&"follow_capped".to_owned()), "{kinds:?}");
    assert!(!kinds.contains(&"follow_requested".to_owned()), "{kinds:?}");
    assert_eq!(ben.body["follows"], json!([]));

    // Ada locked (movies + adult): the same as Ben.
    let locked = p.as_member(&p.ada, get(path)).await;
    assert_eq!(offered(&locked), kinds);
    // Unlocked: the follow kinds of her performer follows appear.
    p.set_unlocked(&p.ada.cookie, true).await;
    let open = p.as_member(&p.ada, get(path)).await;
    assert!(offered(&open).contains(&"follow_capped".to_owned()));
    assert!(offered(&open).contains(&"follow_requested".to_owned()));
    let saved = p
        .as_member(&p.ada, set(json!(["follow_capped", "request_available"])))
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.raw);
    assert_eq!(
        saved.body["email"],
        json!(["follow_capped", "request_available"])
    );

    // Locked again: neither offered nor shown as chosen, and saving the
    // visible kinds keeps the hidden choice; choosing a hidden kind is
    // refused as if it did not exist.
    p.set_unlocked(&p.ada.cookie, false).await;
    let locked = p.as_member(&p.ada, get(path)).await;
    assert!(!locked.raw.contains("follow_capped"), "{}", locked.raw);
    assert_eq!(locked.body["email"], json!(["request_available"]));
    let saved = p.as_member(&p.ada, set(json!(["request_approved"]))).await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.raw);
    assert_eq!(saved.body["email"], json!(["request_approved"]));
    let refused = p.as_member(&p.ada, set(json!(["follow_capped"]))).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    p.set_unlocked(&p.ada.cookie, true).await;
    let open = p.as_member(&p.ada, get(path)).await;
    assert_eq!(
        open.body["email"],
        json!(["follow_capped", "request_approved"])
    );
}

#[tokio::test]
async fn a_member_without_the_entitlement_never_meets_the_category_name() {
    let p = privacy().await;
    for path in documented_gets(&p) {
        let res = get(&path)
            .cookie(&p.ben.cookie)
            .send(&p.setup.env.app)
            .await;
        assert!(
            !res.raw.contains("adult") && !res.raw.contains("NSFW"),
            "ben: GET {path} names the category: {}",
            res.raw
        );
    }
    // Ada locked: her profile does not list it either (the account menu
    // offers the switch, without naming the category).
    let profile = p.as_member(&p.ada, get("/api/v1/me")).await;
    assert!(!profile.raw.contains("adult"), "{}", profile.raw);
    p.set_unlocked(&p.ada.cookie, true).await;
    let profile = p.as_member(&p.ada, get("/api/v1/me")).await;
    assert!(profile.raw.contains("adult"), "{}", profile.raw);
}

#[tokio::test]
async fn switching_adult_or_the_entitlement_closes_open_unlocks() {
    let p = privacy().await;
    let unlocked = async |cookie: &str| me(&p, cookie).await["privateCategory"]["unlocked"].clone();
    p.set_unlocked(&p.ada.cookie, true).await;
    p.set_unlocked(&p.ops, true).await;
    // Off and on again within the 30 minutes: nobody is open by itself.
    p.switch_adult(false).await;
    // Locking still works while the category is off.
    assert_eq!(
        p.set_unlocked(&p.ada.cookie, false).await.status,
        StatusCode::OK
    );
    p.switch_adult(true).await;
    assert_eq!(unlocked(&p.ada.cookie).await, false);
    assert_eq!(unlocked(&p.ops).await, false);

    // The entitlement removed and granted again: Ada's unlock is gone.
    p.set_unlocked(&p.ada.cookie, true).await;
    let path = format!("/api/v1/members/{}/entitlements", p.ada.id);
    let only_movies = json!({ "entitlements": [
        { "category": "movies", "enabled": true, "requestMode": "approval" },
    ]});
    let res = p.admin(put(&path, only_movies)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let both = json!({ "entitlements": [
        { "category": "movies", "enabled": true, "requestMode": "approval" },
        { "category": "adult", "enabled": true, "requestMode": "approval" },
    ]});
    p.admin(put(&path, both)).await;
    assert_eq!(unlocked(&p.ada.cookie).await, false);
    let mine = p.as_member(&p.ada, get("/api/v1/requests")).await;
    assert_clean("ada after the entitlement came back", &mine);
}

#[tokio::test]
async fn adult_switches_on_only_with_the_operator_statement() {
    let p = privacy().await;
    let all = p.admin(get("/api/v1/categories")).await;
    let adult = all
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["category"] == "adult")
        .unwrap()
        .clone();
    assert_eq!(adult["operatorStatement"]["by"], "root", "{adult}");
    let movies = all.body[0].clone();
    assert_eq!(movies["operatorStatement"], Value::Null);

    p.switch_adult(false).await;
    for body in [
        json!({ "enabled": true }),
        json!({ "enabled": true, "operatorStatement": false }),
    ] {
        let res = p.admin(patch("/api/v1/categories/adult", body)).await;
        assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
        assert_eq!(res.code(), "categories.statement_required");
    }
    let res = p
        .ops(patch(
            "/api/v1/categories/adult",
            json!({ "enabled": true, "operatorStatement": true }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["operatorStatement"]["by"], "ops");
    // Other categories never ask.
    let res = p
        .admin(patch(
            "/api/v1/categories/music",
            json!({ "enabled": true }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let (details,): (String,) = sqlx::query_as(
        "SELECT details FROM audit_log WHERE action = 'categories.update' AND target = 'adult'
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(p.pool())
    .await
    .unwrap();
    assert!(details.contains("\"operatorStatement\":true"), "{details}");

    // Switched on before this release (no statement): the gate stays
    // closed until an admin confirms; the switch itself is untouched.
    sqlx::query(
        "UPDATE category SET statement_at = NULL, statement_by = NULL WHERE name = 'adult'",
    )
    .execute(p.pool())
    .await
    .unwrap();
    let res = p.set_unlocked(&p.ada.cookie, true).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    let adult = p.admin(get("/api/v1/categories")).await.body[3].clone();
    assert_eq!(adult["enabled"], true);
    assert_eq!(adult["operatorStatement"], Value::Null);
}

#[tokio::test]
async fn adult_images_answer_not_found_without_cache_or_upstream_and_are_never_stored() {
    let p = privacy().await;
    let image = format!("/api/v1/images/stashdb/300/{IMAGE}");
    for (who, cookie) in [
        ("ben", p.ben.cookie.as_str()),
        ("ada (locked)", p.ada.cookie.as_str()),
        ("root (locked)", p.setup.env.admin.as_str()),
    ] {
        // Invalid paths too: the gate answers before any validation.
        for path in [image.clone(), "/api/v1/images/stashdb/w9/x".to_owned()] {
            let res = get(&path).cookie(cookie).send(&p.setup.env.app).await;
            assert_eq!(res.status, StatusCode::NOT_FOUND, "{who}: {}", res.raw);
            assert_eq!(res.code(), "images.not_found", "{who}");
            // Nothing of it may be stored: errors get `no-store` from the
            // API layer, images `private, no-store` from the route.
            let cache = res.headers.get(header::CACHE_CONTROL).unwrap();
            assert!(
                cache.to_str().unwrap().contains("no-store"),
                "{who}: {cache:?}"
            );
            assert!(res.headers.get(header::ETAG).is_none(), "{who}");
        }
    }
    // Neither the upstream nor the disk was touched.
    assert_eq!(image_requests(&p.stashdb).await, 0);
    assert_eq!(p.images.files(), Vec::<std::path::PathBuf>::new());

    // Open: served, never stored by the browser, kept only in the adult
    // tree, fetched once.
    p.set_unlocked(&p.ops, true).await;
    p.set_unlocked(&p.ada.cookie, true).await;
    for cookie in [p.ops.as_str(), p.ada.cookie.as_str()] {
        let res = get(&image)
            .cookie(cookie)
            .header("if-none-match", "*")
            .send(&p.setup.env.app)
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        assert_eq!(res.raw.as_bytes(), IMAGE_BYTES);
        assert_eq!(res.headers[header::CACHE_CONTROL], "private, no-store");
        assert!(res.headers.get(header::ETAG).is_none());
        assert_eq!(res.headers[header::CONTENT_TYPE], "image/png");
    }
    assert_eq!(image_requests(&p.stashdb).await, 1);
    // The key never travels with an image request.
    for request in p.stashdb.received_requests().await.unwrap() {
        if request.url.path().starts_with("/images/") {
            assert!(request.headers.get("apikey").is_none());
        }
    }
    let files = p.images.files();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].starts_with(p.images.0.join("adult")), "{files:?}");
    let res = get("/api/v1/images/stashdb/w300/x")
        .cookie(&p.ops)
        .send(&p.setup.env.app)
        .await;
    assert_eq!(res.code(), "validation.image_path");

    // Switched off: the gate closes and the adult caches are gone.
    let lane = p
        .as_member(&p.ada, get("/api/v1/private/discover/new"))
        .await;
    assert_eq!(lane.status, StatusCode::OK, "{}", lane.raw);
    let cached = |p: &Privacy| {
        let pool = p.pool().clone();
        async move {
            let (rows,): (i64,) =
                sqlx::query_as("SELECT count(*) FROM catalog_cache WHERE request LIKE 'adult:%'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            rows
        }
    };
    assert!(cached(&p).await > 0);
    // General caches next to them stay (spec 0007 Q13 purges only adult).
    let poster = seed_general_caches(&p).await;
    assert_eq!(p.switch_adult(false).await.status, StatusCode::OK);
    assert_eq!(p.images.files(), vec![poster]);
    assert_eq!(cached(&p).await, 0);
    // The switch also enqueues housekeeping, which repeats the purge in
    // the worker (and again every hour while adult stays off).
    assert_eq!(general_rows_and_due_housekeeping(&p).await, (1, 1));
    let res = get(&image).cookie(&p.ops).send(&p.setup.env.app).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    // Other sources keep their caching (unchanged).
    let res = get("/api/v1/images/nope/w300/x")
        .cookie(&p.ben.cookie)
        .send(&p.setup.env.app)
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

/// A general image and a general cached catalog answer; returns the
/// image's path.
async fn seed_general_caches(p: &Privacy) -> std::path::PathBuf {
    let poster = ImageCache::new(p.images.0.clone())
        .store("tmdb", "w92", "abc", ImageFormat::Jpeg, b"x")
        .unwrap();
    sqlx::query(
        "INSERT INTO catalog_cache (service_instance_id, request, body, fetched_at, expires_at)
         SELECT id, '3/movie/603', x'00', '2026-11-01T00:00:00.000Z', '2999-01-01T00:00:00.000Z'
         FROM service_instance WHERE adapter_type = 'tmdb'",
    )
    .execute(p.pool())
    .await
    .unwrap();
    poster
}

/// Cached catalog answers left, and housekeeping runs due now.
async fn general_rows_and_due_housekeeping(p: &Privacy) -> (i64, i64) {
    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM catalog_cache")
        .fetch_one(p.pool())
        .await
        .unwrap();
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    let (due,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM job WHERE kind = 'system.housekeeping' AND run_after <= ?",
    )
    .bind(now)
    .fetch_one(p.pool())
    .await
    .unwrap();
    (rows, due)
}

#[tokio::test]
async fn global_discover_and_search_never_touch_the_adult_catalog() {
    let p = privacy().await;
    // StashDB is the adult catalog: neither the global Discover nor the
    // global search may ask it, even for an unlocked, entitled member.
    p.set_unlocked(&p.ada.cookie, true).await;
    let before = graphql_requests(&p.stashdb).await;
    let discover = p.as_member(&p.ada, get("/api/v1/discover")).await;
    assert_eq!(discover.status, StatusCode::OK, "{}", discover.raw);
    assert!(
        discover
            .body
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["category"] != "adult"),
        "{}",
        discover.raw
    );
    let search = p.as_member(&p.ada, get("/api/v1/search?query=Scene")).await;
    assert_clean("ada (unlocked): global search", &search);
    for path in [
        "/api/v1/discover/adult/trending",
        "/api/v1/discover/adult/new",
    ] {
        let lane = p.as_member(&p.ada, get(path)).await;
        assert_eq!(lane.status, StatusCode::NOT_FOUND);
        assert_eq!(lane.code(), "discover.unavailable");
    }
    for kind in ["scene", "performer", "studio"] {
        let res = p
            .as_member(&p.ada, get(&format!("/api/v1/titles/{kind}/{SCENE_A}")))
            .await;
        assert_clean(&format!("global title {kind}"), &res);
    }
    assert_eq!(
        graphql_requests(&p.stashdb).await,
        before,
        "the adult catalog was asked"
    );
}

#[tokio::test]
async fn admin_views_are_neutral_until_the_admin_unlocks() {
    let p = privacy().await;
    // Something held at the paused Radarr for the resume confirmation.
    sqlx::query("UPDATE acquisition SET state = 'pending', last_error = NULL WHERE id = ?")
        .bind(p.acquisition)
        .execute(p.pool())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE service_instance SET writes_paused_at = '2026-11-01T11:00:00.000Z' WHERE id = ?",
    )
    .bind(p.setup.radarr)
    .execute(p.pool())
    .await
    .unwrap();
    let ben = p.ben_request;
    let ada = p.ada_request;
    let held = format!("/api/v1/services/{}/held", p.setup.radarr);
    let detail = format!("/api/v1/admin/requests/{ada}");
    for (who, unlocked) in [("root", false), ("ops", true)] {
        let send = async |req: Req| {
            if unlocked {
                p.ops(req).await
            } else {
                p.admin(req).await
            }
        };
        if unlocked {
            p.set_unlocked(&p.ops, true).await;
        }
        let needs_you = send(get("/api/v1/admin/approvals")).await;
        let row = needs_you
            .body
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == ben)
            .unwrap()
            .clone();
        assert_eq!(row["redacted"], !unlocked, "{who}: {row}");
        assert_eq!(row["category"], "adult");
        let request = send(get(&detail)).await;
        let acquisitions = send(get("/api/v1/admin/acquisitions")).await;
        let held = send(get(&held)).await;
        let notifications = send(get("/api/v1/admin/notifications")).await;
        if unlocked {
            assert_eq!(row["member"]["username"], "ben");
            for res in [&request, &acquisitions, &held, &notifications] {
                assert!(names_adult(res), "{who}: {}", res.raw);
            }
        } else {
            assert_eq!(row["member"]["username"], "");
            assert_eq!(row["title"], "");
            assert_eq!(row["poster"], Value::Null);
            assert_eq!(request.body["events"][0]["actor"], Value::Null);
            assert_eq!(held.body["preview"][0]["redacted"], true, "{}", held.raw);
            assert_eq!(notifications.body["items"][0]["redacted"], true);
            // Admins see neutral rows and count them.
            assert_eq!(notifications.body["unread"], 2);
            for (what, res) in [
                ("approvals", &needs_you),
                ("request", &request),
                ("acquisitions", &acquisitions),
                ("held", &held),
                ("notifications", &notifications),
            ] {
                assert_clean(&format!("{who}: {what}"), res);
            }
        }
    }
    // Locked admins may still decide: approving reveals nothing.
    let res = p
        .admin(post(
            "/api/v1/admin/approvals/reject",
            json!({ "requests": [{ "id": ben, "memberId": p.ben.id }], "reason": "Reason R by Studio S" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_clean("reject answer", &res);
}

#[tokio::test]
async fn audit_and_run_reports_keep_ids_only_for_adult() {
    let p = privacy().await;
    p.set_unlocked(&p.ops, true).await;
    p.set_unlocked(&p.ada.cookie, true).await;
    // A reject with a reason that names the title, a retry of the failed
    // acquisition, an unfollow.
    let res = p
        .ops(post(
            "/api/v1/admin/approvals/reject",
            json!({ "requests": [{ "id": p.ben_request, "memberId": p.ben.id }], "reason": "Scene A is Reason R" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = p
        .ops(post(
            &format!("/api/v1/admin/acquisitions/{}/retry", p.acquisition),
            json!({}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    // follows.check over the private follow, before it ends.
    let res = p
        .ops(post("/api/v1/jobs/follows.check/trigger", json!({})))
        .await;
    assert!(res.status.is_success(), "{}", res.raw);
    p.setup.env.drain().await;
    let res = p
        .as_member(
            &p.ada,
            Req::new(
                Method::DELETE,
                &format!("/api/v1/follows/{}", p.ada_follow),
                None,
            ),
        )
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);
    // Submitting fails at the Radarr fake (no such movie).
    p.setup.env.drain().await;

    let rows: Vec<(String, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT action, target, details FROM audit_log")
            .fetch_all(p.pool())
            .await
            .unwrap();
    let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    for action in ["requests.reject", "acquisition.retry", "follow.end"] {
        assert!(
            actions.contains(&action),
            "{action} not audited: {actions:?}"
        );
    }
    for (action, target, details) in &rows {
        let text = format!("{action} {target:?} {details:?}");
        for marker in MARKERS {
            assert!(!text.contains(marker), "audit names {marker:?}: {text}");
        }
    }
    let runs: Vec<(i64, String, Option<String>)> = sqlx::query_as(
        "SELECT id, kind, report FROM run WHERE kind IN ('requests.submit', 'follows.check')",
    )
    .fetch_all(p.pool())
    .await
    .unwrap();
    for kind in ["requests.submit", "follows.check"] {
        assert!(runs.iter().any(|r| r.1 == kind), "{kind} did not run");
    }
    let mut named_by_id = false;
    let mut follow_by_id = false;
    for (id, _, report) in &runs {
        let report = report.as_deref().unwrap_or_default();
        named_by_id |= report.contains(&format!("acquisition #{}", p.acquisition));
        follow_by_id |= report.contains(&format!("follow #{}", p.ada_follow));
        assert!(!report.contains("ada:"), "the member is named: {report}");
        // Run history shows ids only, for an unlocked admin too (ADR 0014).
        let res = p.ops(get(&format!("/api/v1/runs/{id}"))).await;
        for text in [report, res.raw.as_str()] {
            for marker in MARKERS {
                assert!(
                    !text.contains(marker),
                    "run report names {marker:?}: {text}"
                );
            }
        }
    }
    assert!(named_by_id, "the private unit is named by its id: {runs:?}");
    assert!(
        follow_by_id,
        "the private follow is named by its id: {runs:?}"
    );
    let list = p.ops(get("/api/v1/runs")).await;
    assert_clean("ops (unlocked): runs", &list);
}
/// Scene requests (spec 0007 slice 4): made through the private area, they
/// never show outside the gate. Ben (not entitled), Ada locked and Root
/// locked sweep every documented `GET` and find neither the scene's title
/// nor its `StashDB` UUID; the private routes answer 404 while closed (also
/// to `POST`), actions on the hidden scene request answer 404, and the
/// general routes never take a scene.
#[tokio::test]
async fn scene_requests_stay_behind_the_gate() {
    let p = privacy().await;
    let (_whisparr, server) = common::whisparr::fake_whisparr(&[(SCENE_A, "Scene A")]).await;
    common::whisparr::connect_whisparr(&p.setup.env, &server).await;
    p.set_unlocked(&p.ada.cookie, true).await;
    let res = p
        .as_member(
            &p.ada,
            post("/api/v1/private/requests", json!({ "scenes": [SCENE_A] })),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let id = res.body["requests"][0]["id"].as_i64().unwrap();
    p.set_unlocked(&p.ada.cookie, false).await;
    let root = p.setup.env.admin.clone();

    for (who, cookie) in [
        ("ben", p.ben.cookie.as_str()),
        ("ada (locked)", p.ada.cookie.as_str()),
        ("root (locked)", root.as_str()),
    ] {
        sweep(&p, who, cookie).await;
        for path in documented_gets(&p) {
            let res = get(&path).cookie(cookie).send(&p.setup.env.app).await;
            assert!(
                !res.raw.contains(SCENE_A),
                "{who}: GET {path} names the scene: {}",
                res.raw
            );
        }
    }
    for member in [&p.ben, &p.ada] {
        let res = p
            .as_member(
                member,
                post("/api/v1/private/requests", json!({ "scenes": [SCENE_A] })),
            )
            .await;
        assert_eq!(res.code(), "private.not_found", "{}", res.raw);
        let res = p
            .as_member(
                member,
                post(
                    "/api/v1/requests",
                    json!({ "kind": "scene", "titleId": SCENE_A }),
                ),
            )
            .await;
        assert_eq!(res.code(), "catalog.not_found", "{}", res.raw);
    }
    // Ada's own scene request, hidden by her locked session.
    for req in [
        post(&format!("/api/v1/requests/{id}/cancel"), json!({})),
        post(&format!("/api/v1/requests/{id}/withdraw"), json!({})),
        put(
            &format!("/api/v1/requests/{id}/hidden"),
            json!({ "hidden": true }),
        ),
    ] {
        let res = p.as_member(&p.ada, req).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
        assert_eq!(res.code(), "requests.not_found");
    }
    // Root approves the neutral row; Ops, unlocked, sees the scene.
    let res = p
        .admin(post(
            "/api/v1/admin/approvals/approve",
            json!({ "requests": [{ "id": id, "memberId": p.ada.id }] }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_clean("root: approve", &res);
    p.set_unlocked(&p.ops, true).await;
    let detail = p.ops(get(&format!("/api/v1/admin/requests/{id}"))).await;
    assert_eq!(detail.body["request"]["kind"], "scene", "{}", detail.raw);
    assert_eq!(detail.body["request"]["titleId"], SCENE_A);
    let detail = p.admin(get(&format!("/api/v1/admin/requests/{id}"))).await;
    assert_eq!(detail.body["request"]["redacted"], true, "{}", detail.raw);
    assert!(!detail.raw.contains(SCENE_A), "{}", detail.raw);
    // Switched off, the private routes are gone for everyone.
    assert_eq!(p.switch_adult(false).await.status, StatusCode::OK);
    p.set_unlocked(&p.ada.cookie, true).await;
    let res = p.as_member(&p.ada, get("/api/v1/private/requests")).await;
    assert_eq!(res.code(), "private.not_found", "{}", res.raw);
}
/// Spec 0008 slice 2: a scene's description is treated like its title. It
/// reaches only an entitled member through the open gate (closed: the
/// scene answers not found and the sweep finds it nowhere), is stored only
/// in the `adult:` catalog cache (never in `catalog_item`, notifications,
/// jobs, runs or the audit log, also after the scene is requested), and is
/// purged with that cache when the category is switched off.
#[tokio::test]
async fn scene_descriptions_stay_behind_the_gate_and_in_the_adult_cache() {
    const DETAILS: &str = "Details D";
    let p = privacy().await;
    let scene = format!("/api/v1/private/scenes/{SCENE_A}");

    // Closed gate: not found, and no answer anywhere names it.
    let res = p.as_member(&p.ada, get(&scene)).await;
    assert_eq!(res.code(), "private.not_found", "{}", res.raw);
    assert!(!res.raw.contains(DETAILS));
    let root = p.setup.env.admin.clone();
    for (who, cookie) in [
        ("ben", p.ben.cookie.as_str()),
        ("ada (locked)", p.ada.cookie.as_str()),
        ("root (locked)", root.as_str()),
    ] {
        sweep(&p, who, cookie).await;
    }

    // Open: the scene page shows it, as plain text.
    p.set_unlocked(&p.ada.cookie, true).await;
    let res = p.as_member(&p.ada, get(&scene)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let details = res.body["details"].as_str().unwrap();
    assert!(details.starts_with(DETAILS), "{details}");
    assert!(!details.contains('\u{7}'), "control characters are dropped");
    // Lists and search never carry it.
    for res in [
        p.as_member(&p.ada, get("/api/v1/private/discover/new"))
            .await,
        p.as_member(
            &p.ada,
            post("/api/v1/private/search", json!({ "query": "Scene" })),
        )
        .await,
    ] {
        assert!(!res.raw.contains(DETAILS), "{}", res.raw);
    }

    // Requested (recorded in `catalog_item`), the description stays out.
    let (_whisparr, server) = common::whisparr::fake_whisparr(&[(SCENE_A, "Scene A")]).await;
    common::whisparr::connect_whisparr(&p.setup.env, &server).await;
    let res = p
        .as_member(
            &p.ada,
            post("/api/v1/private/requests", json!({ "scenes": [SCENE_A] })),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    p.setup.env.drain().await;

    let cached: Vec<(String, Vec<u8>)> = sqlx::query_as("SELECT request, body FROM catalog_cache")
        .fetch_all(p.pool())
        .await
        .unwrap();
    let holding: Vec<&String> = cached
        .iter()
        .filter(|(_, body)| String::from_utf8_lossy(body).contains(DETAILS))
        .map(|(key, _)| key)
        .collect();
    assert_ne!(holding.len(), 0, "the scene page was not cached");
    for key in &holding {
        assert!(
            key.starts_with(starr_adapter_stashdb::CACHE_PREFIX),
            "{key}"
        );
    }
    for (table, sql) in [
        (
            "catalog_item",
            "SELECT COALESCE(title, '') || COALESCE(metadata, '') FROM catalog_item",
        ),
        ("notification", "SELECT payload FROM notification"),
        ("job", "SELECT payload FROM job"),
        ("run", "SELECT COALESCE(report, '') FROM run"),
        (
            "audit_log",
            "SELECT action || COALESCE(target, '') || COALESCE(details, '') FROM audit_log",
        ),
    ] {
        let rows: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(sql.to_owned()))
            .fetch_all(p.pool())
            .await
            .unwrap();
        for (text,) in rows {
            assert!(!text.contains(DETAILS), "{table}: {text}");
        }
    }

    // Switched off, the adult cache (with the description) is purged.
    assert_eq!(p.switch_adult(false).await.status, StatusCode::OK);
    let (left,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM catalog_cache WHERE CAST(body AS TEXT) LIKE '%Details D%'",
    )
    .fetch_one(p.pool())
    .await
    .unwrap();
    assert_eq!(left, 0);
}

/// Spec 0008 F9, PRIV-08: the Inbox lists and counts private items as
/// neutral rows for a locked admin (the count is the same before and
/// after unlocking); unlocked, the same rows name the title.
#[tokio::test]
async fn the_inbox_counts_private_items_as_neutral_rows_until_unlocked() {
    let p = privacy().await;
    let root = p.setup.env.admin.clone();
    let locked = get("/api/v1/admin/attention")
        .cookie(&root)
        .send(&p.setup.env.app)
        .await;
    assert_eq!(locked.status, StatusCode::OK, "{}", locked.raw);
    assert_clean("root (locked): attention", &locked);
    let items = locked.body["items"].as_array().unwrap();
    let approval = items
        .iter()
        .find(|i| i["id"] == format!("approval:{}", p.ben_request))
        .unwrap();
    assert_eq!(approval["redacted"], true);
    assert_eq!(approval["request"]["title"], "");
    let failed = items
        .iter()
        .find(|i| i["id"] == format!("failed:{}", p.acquisition))
        .unwrap();
    assert_eq!(failed["redacted"], true);
    assert_eq!(failed["acquisition"]["lastError"], Value::Null);

    // "Show to decide" unlocks in place: the same rows, the same count.
    assert_eq!(p.set_unlocked(&p.ops, true).await.status, StatusCode::OK);
    let open = p.ops(get("/api/v1/admin/attention")).await;
    assert_eq!(open.body["total"], locked.body["total"], "{}", open.raw);
    assert!(names_adult(&open), "{}", open.raw);
    let approval = open.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == format!("approval:{}", p.ben_request))
        .unwrap();
    assert_eq!(approval["redacted"], false);
}
