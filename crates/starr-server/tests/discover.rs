//! Discover, search and title details for members (spec 0003 slice 5).
//! Personal data (what a member requested) is checked with two members
//! against the same title (lesson 4); images only as proxy paths (O4).
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;
use common::media::{MOVIE, SERIES, Setup, member, put, setup};
use common::services::patch;
use common::{TestResponse, elevate_in_place, get, post};
use serde_json::{Value, json};

/// Save `relevance` on the default catalog of `category`, which makes
/// Discover ready there (D3).
async fn make_ready_with(s: &Setup, category: &str, relevance: Value) {
    let categories = s.admin(get("/api/v1/categories")).await;
    let catalog = categories
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["category"]["name"] == category || c["category"] == category)
        .and_then(|c| c["catalogs"].as_array())
        .and_then(|c| c.first())
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let res = s
        .admin(put(
            &format!("/api/v1/categories/{category}/roles/{catalog}/relevance"),
            relevance,
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
}

async fn make_ready(s: &Setup, category: &str) {
    make_ready_with(s, category, json!({})).await;
}

/// Both categories ready; series hide Talk (genre 10767) as by default (D4).
async fn ready() -> Setup {
    let s = setup().await;
    make_ready(&s, "movies").await;
    make_ready_with(&s, "series", json!({ "excludedGenres": [10767] })).await;
    s
}

fn tmdb_ids(res: &TestResponse) -> Vec<u64> {
    res.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().parse::<u64>().unwrap())
        .collect()
}

fn find(items: &Value, tmdb_id: u64) -> &Value {
    items
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"].as_str() == Some(tmdb_id.to_string().as_str()))
        .unwrap()
}

#[tokio::test]
async fn only_entitled_categories_appear_and_need_a_relevance() {
    let s = setup().await;
    let lena = member(
        &s.env,
        "lena",
        json!([{ "category": "movies", "enabled": true, "requestMode": "approval" }]),
    )
    .await;

    let res = s.as_member(&lena, get("/api/v1/discover")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body.as_array().unwrap().len(), 1, "{}", res.raw);
    assert_eq!(res.body[0]["category"], "movies");
    assert_eq!(res.body[0]["kind"], "movie");
    assert_eq!(res.body[0]["discoverReady"], false);
    // Lists of a category that is not ready are refused.
    let res = s
        .as_member(&lena, get("/api/v1/discover/movies/trending"))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.code(), "discover.unavailable");

    make_ready(&s, "movies").await;
    let res = s.as_member(&lena, get("/api/v1/discover")).await;
    assert_eq!(res.body[0]["discoverReady"], true);
    assert_eq!(res.body[0]["requestable"], true);
    // "On your services" needs providers, which this relevance has none of.
    assert_eq!(
        res.body[0]["lists"],
        json!(["trending", "popular", "upcoming", "top_rated"])
    );
    // Series: not entitled, so not listed and not reachable.
    let res = s
        .as_member(&lena, get("/api/v1/discover/series/popular"))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    let res = s
        .as_member(&lena, get(&format!("/api/v1/titles/series/{SERIES}")))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

    // Mia has both; series has no relevance yet.
    let res = s.as_member(&s.mia, get("/api/v1/discover")).await;
    let ready: Vec<_> = res
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["category"].as_str().unwrap(),
                c["discoverReady"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(ready, vec![("movies", true), ("series", false)]);
}

#[tokio::test]
async fn admins_and_anonymous_callers_are_refused() {
    let s = ready().await;
    for path in [
        "/api/v1/discover".to_owned(),
        "/api/v1/discover/movies/trending".to_owned(),
        "/api/v1/search?query=inception".to_owned(),
        format!("/api/v1/titles/movie/{MOVIE}"),
    ] {
        let res = s.admin(get(&path)).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{path}: {}", res.raw);
        let res = get(&path).send(&s.env.app).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{path}");
    }
}

#[tokio::test]
async fn list_pages_carry_proxy_paths_and_status_without_names() {
    let s = ready().await;
    let noah = s.request_movie(&s.noah).await;
    assert_eq!(noah.status, StatusCode::CREATED, "{}", noah.raw);

    let mia = s
        .as_member(&s.mia, get("/api/v1/discover/movies/trending"))
        .await;
    assert_eq!(mia.status, StatusCode::OK, "{}", mia.raw);
    assert_eq!(tmdb_ids(&mia)[0], MOVIE);
    assert_eq!(mia.body["next"], 2);
    let inception = find(&mia.body["items"], MOVIE);
    assert_eq!(inception["title"], "Inception");
    assert_eq!(inception["kind"], "movie");
    assert_eq!(inception["year"], 2010);
    // Noah's request: "requested" for Mia, but not hers and not his name.
    assert_eq!(inception["status"], "requested");
    assert_eq!(inception["requestedByMe"], false);
    assert!(!mia.raw.contains("noah"), "{}", mia.raw);
    // Images only as file names for Starr's proxy.
    let poster = inception["poster"].as_str().unwrap();
    assert!(
        !poster.contains('/')
            && std::path::Path::new(poster)
                .extension()
                .is_some_and(|e| e == "jpg"),
        "{poster}"
    );
    assert!(!mia.raw.contains("image.tmdb.org"), "{}", mia.raw);

    let noah = s
        .as_member(&s.noah, get("/api/v1/discover/movies/trending?page=1"))
        .await;
    assert_eq!(find(&noah.body["items"], MOVIE)["requestedByMe"], true);

    // The relevance applies: the Tonight Show (Talk) is filtered.
    let series = s
        .as_member(&s.mia, get("/api/v1/discover/series/popular"))
        .await;
    assert_eq!(series.status, StatusCode::OK, "{}", series.raw);
    let ids = tmdb_ids(&series);
    assert!(ids.contains(&SERIES), "{ids:?}");
    assert!(!ids.contains(&59941), "{ids:?}");
    assert_eq!(find(&series.body["items"], SERIES)["status"], "new");
}

#[tokio::test]
async fn invalid_list_requests_are_refused() {
    let s = ready().await;
    for (path, status, code) in [
        (
            "/api/v1/discover/movies/latest",
            StatusCode::NOT_FOUND,
            "discover.unknown_list",
        ),
        (
            "/api/v1/discover/music/trending",
            StatusCode::NOT_FOUND,
            "discover.unavailable",
        ),
        (
            "/api/v1/discover/nope/trending",
            StatusCode::NOT_FOUND,
            "discover.unavailable",
        ),
        (
            "/api/v1/discover/movies/trending?page=0",
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation.page",
        ),
        // TMDB serves 500 pages at most.
        (
            "/api/v1/discover/movies/trending?page=501",
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation.page",
        ),
        // "On your services" needs providers in the relevance.
        (
            "/api/v1/discover/movies/on_services",
            StatusCode::NOT_FOUND,
            "discover.unknown_list",
        ),
        (
            "/api/v1/search?query=inception&page=0",
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation.page",
        ),
    ] {
        let res = s.as_member(&s.mia, get(path)).await;
        assert_eq!(res.status, status, "{path}: {}", res.raw);
        assert_eq!(res.code(), code, "{path}");
    }
    let long = "x".repeat(201);
    let res = s
        .as_member(&s.mia, get(&format!("/api/v1/search?query={long}")))
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(res.code(), "validation.search_query");
}

#[tokio::test]
async fn search_finds_movies_and_series_of_entitled_categories_only() {
    let s = ready().await;
    let mia = s
        .as_member(&s.mia, get("/api/v1/search?query=inception"))
        .await;
    assert_eq!(mia.status, StatusCode::OK, "{}", mia.raw);
    let kinds: Vec<_> = mia.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["kind"].as_str().unwrap(),
                i["id"].as_str().unwrap().parse::<u64>().unwrap(),
            )
        })
        .collect();
    // No people (D12).
    assert_eq!(kinds, vec![("movie", MOVIE), ("series", MOVIE)]);
    assert_eq!(mia.body["next"], Value::Null);
    assert_eq!(mia.body["items"][0]["status"], "new");

    // Nothing to search for: an empty page, no catalog call needed.
    let blank = s.as_member(&s.mia, get("/api/v1/search?query=%20")).await;
    assert_eq!(blank.status, StatusCode::OK, "{}", blank.raw);
    assert_eq!(blank.body["items"], json!([]));

    let lena = member(
        &s.env,
        "lena",
        json!([{ "category": "movies", "enabled": true, "requestMode": "approval" }]),
    )
    .await;
    let res = s
        .as_member(&lena, get("/api/v1/search?query=inception"))
        .await;
    assert_eq!(tmdb_ids(&res), vec![MOVIE]);
    assert_eq!(res.body["items"][0]["kind"], "movie");
}

#[tokio::test]
async fn movie_details_have_trailer_rating_and_open_in_links() {
    let s = ready().await;
    let path = format!("/api/v1/titles/movie/{MOVIE}");
    let res = s.as_member(&s.mia, get(&path)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let d = &res.body;
    assert_eq!(d["title"], "Inception");
    assert_eq!(d["year"], 2010);
    assert_eq!(d["category"], "movies");
    assert!(d["overview"].as_str().unwrap().len() > 20);
    assert!(d["rating"].as_f64().unwrap() > 8.0);
    // The first official YouTube trailer, as a plain link (D13).
    assert_eq!(
        d["trailerUrl"],
        "https://www.youtube.com/watch?v=YoHD9XEInc0"
    );
    assert_eq!(d["status"], "new");
    assert_eq!(d["requestable"], true);
    assert_eq!(d["seasons"], json!([]));
    assert_eq!(d["openIn"], json!([]));

    // Jellyfin watches movies and has the title, first without a public URL.
    let res = s
        .admin(post(
            "/api/v1/services",
            json!({ "adapterType": "jellyfin", "name": "Kino", "baseUrl": "http://jellyfin.internal:8096",
                    "apiKey": "k" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    let jellyfin = res.body["service"]["id"].as_i64().unwrap();
    let res = s
        .admin(post(
            "/api/v1/categories/movies/roles",
            json!({ "kind": "consume", "serviceId": jellyfin }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    sqlx::query(
        "INSERT INTO inventory (service_instance_id, natural_key, kind, tmdb_id, remote_id,
             server_id, title, present_since, last_seen)
         VALUES (?, 'movie:tmdb:27205', 'movie', 27205, 'abc', 'srv1', 'Inception',
             '2026-11-01T00:00:00.000Z', '2026-11-01T00:00:00.000Z')",
    )
    .bind(jellyfin)
    .execute(s.env.app.state.db.pool())
    .await
    .unwrap();

    // Available, but no link: the base URL is internal (maintainer, 2026-11-02).
    let res = s.as_member(&s.mia, get(&path)).await;
    assert_eq!(res.body["status"], "available");
    assert_eq!(res.body["openIn"], json!([]));
    assert!(!res.raw.contains("jellyfin.internal"), "{}", res.raw);

    // With a public URL, the link points there.
    let res = s
        .admin(patch(
            &format!("/api/v1/services/{jellyfin}"),
            json!({ "publicUrl": "https://watch.example.com" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = s.as_member(&s.mia, get(&path)).await;
    assert_eq!(
        res.body["openIn"],
        json!([{
            "service": "jellyfin",
            "name": "Kino",
            "url": "https://watch.example.com/web/#/details?id=abc&serverId=srv1",
        }])
    );
    // The internal address never reaches members.
    assert!(!res.raw.contains("jellyfin.internal"), "{}", res.raw);

    // Unknown titles are not found.
    let res = s.as_member(&s.mia, get("/api/v1/titles/movie/1")).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
    let res = s
        .as_member(&s.mia, get("/api/v1/titles/movie/not-a-tmdb-id"))
        .await;
    assert_eq!(res.code(), "catalog.not_found", "{}", res.raw);
}

/// Maintainer's acceptance test: a movie Radarr has but Jellyfin does not
/// list yet shows as arriving in Jellyfin, not as "Downloading 100%".
#[tokio::test]
async fn a_movie_in_the_source_but_not_the_library_shows_as_arriving() {
    let s = ready().await;
    let res = s
        .admin(post(
            "/api/v1/services",
            json!({ "adapterType": "jellyfin", "name": "Kino", "baseUrl": "http://jellyfin.internal:8096",
                    "apiKey": "k" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    let jellyfin = res.body["service"]["id"].as_i64().unwrap();
    let res = s
        .admin(post(
            "/api/v1/categories/movies/roles",
            json!({ "kind": "consume", "serviceId": jellyfin }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let noah = s.request_movie(&s.noah).await;
    assert_eq!(noah.status, StatusCode::CREATED, "{}", noah.raw);
    // What `acquisitions.sync` records for it (worker tests cover that).
    sqlx::query(
        "UPDATE acquisition SET state = 'downloading', progress = 1.0,
             download_stage = 'arriving';
         UPDATE request SET state = 'in_progress';",
    )
    .execute(s.env.app.state.db.pool())
    .await
    .unwrap();

    let mine = s.as_member(&s.noah, get("/api/v1/requests")).await;
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.raw);
    assert_eq!(mine.body[0]["state"], "in_progress");
    assert_eq!(mine.body[0]["stage"], "arriving");
    assert_eq!(mine.body[0]["arrivingIn"], "Kino");
    assert_eq!(mine.body[0]["items"][0]["stage"], "arriving");

    let acquisitions = s.admin(get("/api/v1/admin/acquisitions")).await;
    assert_eq!(acquisitions.body[0]["state"], "downloading");
    assert_eq!(acquisitions.body[0]["stage"], "arriving");
    assert_eq!(acquisitions.body[0]["arrivingIn"], "Kino");
    let requests = s.admin(get("/api/v1/admin/requests")).await;
    assert_eq!(requests.body[0]["stage"], "arriving");
    assert_eq!(requests.body[0]["arrivingIn"], "Kino");
    assert_eq!(requests.body[0]["items"][0]["stage"], "arriving");
    assert_eq!(requests.body[0]["items"][0]["arrivingIn"], "Kino");

    // The detail view says so too, for anyone (without names).
    let path = format!("/api/v1/titles/movie/{MOVIE}");
    let details = s.as_member(&s.mia, get(&path)).await;
    assert_eq!(details.status, StatusCode::OK, "{}", details.raw);
    assert_eq!(details.body["status"], "requested");
    assert_eq!(details.body["stage"], "arriving");
    assert_eq!(details.body["arrivingIn"], "Kino");

    // Importing names no library.
    sqlx::query("UPDATE acquisition SET download_stage = 'importing'")
        .execute(s.env.app.state.db.pool())
        .await
        .unwrap();
    let mine = s.as_member(&s.noah, get("/api/v1/requests")).await;
    assert_eq!(mine.body[0]["stage"], "importing");
    assert_eq!(mine.body[0]["arrivingIn"], Value::Null);

    // Available: no stage, whatever the column says.
    sqlx::query("UPDATE acquisition SET state = 'available', download_stage = 'arriving'")
        .execute(s.env.app.state.db.pool())
        .await
        .unwrap();
    let acquisitions = s.admin(get("/api/v1/admin/acquisitions")).await;
    assert_eq!(acquisitions.body[0]["stage"], Value::Null);
    assert_eq!(acquisitions.body[0]["arrivingIn"], Value::Null);
}

#[tokio::test]
async fn series_details_show_each_season_for_each_member() {
    let s = ready().await;
    let mia = s.request_seasons(&s.mia, &[1]).await;
    assert_eq!(mia.status, StatusCode::CREATED, "{}", mia.raw);

    let path = format!("/api/v1/titles/series/{SERIES}");
    let res = s.as_member(&s.mia, get(&path)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["title"], "Game of Thrones");
    assert_eq!(
        res.body["trailerUrl"],
        "https://www.youtube.com/watch?v=rlR4PJn8b8I"
    );
    let seasons = res.body["seasons"].as_array().unwrap();
    let numbers: Vec<_> = seasons
        .iter()
        .map(|s| s["number"].as_u64().unwrap())
        .collect();
    assert_eq!(numbers, vec![0, 1, 2, 9]);
    assert_eq!(seasons[0]["special"], true);
    assert_eq!(seasons[1]["episodeCount"], 10);
    assert_eq!(seasons[1]["airDate"], "2011-04-17");
    assert_eq!(seasons[1]["status"], "requested");
    assert_eq!(seasons[1]["requestedByMe"], true);
    assert_eq!(seasons[2]["status"], "new");
    assert_eq!(res.body["requestedByMe"], true);

    // Noah sees season 1 as requested, without it being his or Mia's name.
    let res = s.as_member(&s.noah, get(&path)).await;
    let seasons = res.body["seasons"].as_array().unwrap();
    assert_eq!(seasons[1]["status"], "requested");
    assert_eq!(seasons[1]["requestedByMe"], false);
    assert_eq!(res.body["requestedByMe"], false);
    assert!(!res.raw.contains("mia"), "{}", res.raw);
}

#[tokio::test]
async fn switched_off_categories_and_entitlements_close_discover() {
    let s = ready().await;
    // The admin switches movies off: still listed for Mia, but not ready.
    let res = s
        .admin(patch(
            "/api/v1/categories/movies",
            json!({ "enabled": false }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = s.as_member(&s.mia, get("/api/v1/discover")).await;
    assert_eq!(res.body[0]["category"], "movies");
    assert_eq!(res.body[0]["discoverReady"], false);
    assert_eq!(res.body[0]["lists"], json!([]));
    let res = s
        .as_member(&s.mia, get("/api/v1/discover/movies/trending"))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.code(), "discover.unavailable");
    // Search leaves the switched-off category out.
    let res = s
        .as_member(&s.mia, get("/api/v1/search?query=inception"))
        .await;
    let kinds: Vec<_> = res.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["series"]);

    // Series entitlement switched off for Mia: the category disappears.
    let res = s
        .admin(put(
            &format!("/api/v1/members/{}/entitlements", s.mia.id),
            json!({ "entitlements": [
                { "category": "movies", "enabled": true, "requestMode": "approval" },
                { "category": "series", "enabled": false, "requestMode": "approval" },
            ] }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = s.as_member(&s.mia, get("/api/v1/discover")).await;
    let names: Vec<_> = res
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["category"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["movies"]);
    let res = s
        .as_member(&s.mia, get(&format!("/api/v1/titles/series/{SERIES}")))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    // Noah keeps both.
    let res = s.as_member(&s.noah, get("/api/v1/discover")).await;
    assert_eq!(res.body.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn an_unreachable_catalog_is_reported_not_hidden() {
    let s = ready().await;
    let services = s.admin(get("/api/v1/services")).await;
    let tmdb = services
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|svc| svc["adapterType"] == "tmdb")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    // Nothing listens on port 9 (discard).
    elevate_in_place(&s.env.app, &s.env.admin).await;
    let res = s
        .admin(patch(
            &format!("/api/v1/services/{tmdb}"),
            json!({ "baseUrl": "http://127.0.0.1:9", "apiKey": common::media::TMDB_TOKEN }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    for path in [
        "/api/v1/discover/movies/trending".to_owned(),
        "/api/v1/search?query=inception".to_owned(),
        format!("/api/v1/titles/movie/{MOVIE}"),
    ] {
        let res = s.as_member(&s.mia, get(&path)).await;
        assert_eq!(
            res.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{path}: {}",
            res.raw
        );
        assert_eq!(res.code(), "catalog.unavailable", "{path}");
    }
}

/// Status journey (maintainer, 2026-11-02): cards and the detail view carry
/// the journeys of the member's own open requests only.
#[tokio::test]
async fn title_journeys_are_the_members_own() {
    let s = ready().await;
    let noah = s.request_movie(&s.noah).await;
    assert_eq!(noah.status, StatusCode::CREATED, "{}", noah.raw);

    let mia = s
        .as_member(&s.mia, get("/api/v1/discover/movies/trending"))
        .await;
    assert_eq!(find(&mia.body["items"], MOVIE)["journeys"], json!([]));
    let details = s
        .as_member(&s.mia, get(&format!("/api/v1/titles/movie/{MOVIE}")))
        .await;
    assert_eq!(details.body["journeys"], json!([]));

    let noah = s
        .as_member(&s.noah, get("/api/v1/discover/movies/trending"))
        .await;
    let journeys = &find(&noah.body["items"], MOVIE)["journeys"];
    assert_eq!(journeys[0]["season"], Value::Null);
    assert_eq!(
        journeys[0]["journey"]["steps"][1]["step"],
        "approved_automatically"
    );
    let details = s
        .as_member(&s.noah, get(&format!("/api/v1/titles/movie/{MOVIE}")))
        .await;
    assert_eq!(details.body["journeys"].as_array().unwrap().len(), 1);

    // Series: one journey per requested season.
    let res = s.request_seasons(&s.mia, &[1, 2]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let details = s
        .as_member(&s.mia, get(&format!("/api/v1/titles/series/{SERIES}")))
        .await;
    let seasons: Vec<u64> = details.body["journeys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["season"].as_u64().unwrap())
        .collect();
    assert_eq!(seasons, vec![1, 2]);
    assert!(!details.raw.contains("noah"), "{}", details.raw);
}
/// Spec 0008 slice 2: the category page data of a ready category — its
/// provider switcher (relevance providers first, credited to `JustWatch`)
/// and genres without the excluded ones.
#[tokio::test]
async fn categories_carry_providers_and_genres_for_their_pages() {
    let s = setup().await;
    make_ready_with(
        &s,
        "movies",
        json!({ "providerRegion": "CH", "providers": [8] }),
    )
    .await;
    make_ready_with(&s, "series", json!({ "excludedGenres": [10767] })).await;
    let res = s.as_member(&s.mia, get("/api/v1/discover")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let movies = &res.body[0];
    let providers: Vec<u64> = movies["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_u64().unwrap())
        .collect();
    assert_eq!(providers, [8, 337, 1823, 9]);
    assert_eq!(movies["attributions"][0]["source"], "JustWatch");
    assert!(
        movies["genres"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["id"] == "18" && g["name"] == "Drama"),
        "{}",
        res.raw
    );
    // No provider region: no switcher, no credit; Talk is excluded.
    let series = &res.body[1];
    assert_eq!(series["providers"], json!([]));
    assert_eq!(series["attributions"], json!([]));
    let genres = series["genres"].as_array().unwrap();
    assert_ne!(genres.len(), 0);
    assert!(genres.iter().all(|g| g["id"] != "10767"), "{genres:?}");
    // The lanes of the home are unchanged.
    assert_eq!(
        movies["lists"],
        json!([
            "trending",
            "popular",
            "upcoming",
            "on_services",
            "top_rated"
        ])
    );
}

/// The Discover hero: movies and series interleaved by Trending rank,
/// only titles with a backdrop, each a full card.
#[tokio::test]
async fn highlights_interleave_trending_movies_and_series() {
    let s = ready().await;
    let res = s
        .as_member(&s.mia, get("/api/v1/discover/highlights"))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let items = res.body.as_array().unwrap();
    assert_eq!(items.len(), 10, "{}", res.raw);
    let order: Vec<_> = items
        .iter()
        .take(4)
        .map(|h| (h["category"].as_str().unwrap(), h["rank"].as_u64().unwrap()))
        .collect();
    assert_eq!(
        order,
        [("movies", 1), ("series", 1), ("movies", 2), ("series", 2)]
    );
    assert_eq!(items[0]["id"], MOVIE.to_string());
    assert!(items.iter().all(|h| h["backdrop"].is_string()));
    assert_eq!(items[0]["genres"], json!(["Action", "Science Fiction"]));
    assert!(items[0]["overview"].as_str().unwrap().chars().count() <= 200);
    assert_eq!(items[0]["status"], "new");

    let movies = s
        .as_member(&s.mia, get("/api/v1/discover/highlights?category=movies"))
        .await;
    let ids: Vec<_> = movies
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["category"].as_str().unwrap())
        .collect();
    assert_ne!(ids.len(), 0);
    assert!(ids.iter().all(|c| *c == "movies"));
    for path in [
        "/api/v1/discover/highlights?category=music",
        "/api/v1/discover/highlights?category=adult",
        "/api/v1/discover/highlights?category=nope",
    ] {
        let res = s.as_member(&s.mia, get(path)).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{path}: {}", res.raw);
        assert_eq!(res.code(), "discover.unavailable");
    }
}

/// "See all": category page lists, filters, sorts and "Hide what I have".
#[tokio::test]
async fn see_all_lists_take_provider_genre_sort_and_hide_owned() {
    let s = setup().await;
    make_ready_with(
        &s,
        "movies",
        json!({ "providerRegion": "CH", "providers": [8] }),
    )
    .await;
    make_ready_with(&s, "series", json!({ "excludedGenres": [10767] })).await;
    let on = s
        .as_member(
            &s.mia,
            get("/api/v1/discover/movies/on_provider?provider=337"),
        )
        .await;
    assert_eq!(on.status, StatusCode::OK, "{}", on.raw);
    assert_eq!(tmdb_ids(&on)[0], 157_336);
    let genre = s
        .as_member(
            &s.mia,
            get("/api/v1/discover/movies/genre?genre=18&sort=rating"),
        )
        .await;
    assert_eq!(genre.status, StatusCode::OK, "{}", genre.raw);
    let air = get("/api/v1/discover/series/on_the_air");
    let air = s.as_member(&s.mia, air).await.body["items"].clone();
    assert_ne!(air.as_array().unwrap().len(), 0);
    // Hide what I have: Inception is in a library.
    sqlx::query(
        "INSERT INTO inventory (service_instance_id, natural_key, kind, tmdb_id, remote_id,
             server_id, title, present_since, last_seen)
         SELECT id, 'movie:tmdb:27205', 'movie', 27205, 'abc', 'srv1', 'Inception',
             '2026-11-01T00:00:00.000Z', '2026-11-01T00:00:00.000Z'
         FROM service_instance WHERE adapter_type = 'radarr'",
    )
    .execute(s.env.app.state.db.pool())
    .await
    .ok();
    let all = s
        .as_member(&s.mia, get("/api/v1/discover/movies/trending"))
        .await;
    let hidden = s
        .as_member(
            &s.mia,
            get("/api/v1/discover/movies/trending?hideOwned=true"),
        )
        .await;
    assert_eq!(hidden.status, StatusCode::OK, "{}", hidden.raw);
    let available = all.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["status"] == "available")
        .count();
    assert_eq!(
        hidden.body["items"].as_array().unwrap().len() + available,
        all.body["items"].as_array().unwrap().len()
    );
    assert!(
        hidden.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["status"] != "available")
    );
    assert_eq!(hidden.body["next"], all.body["next"]);
    for (path, code) in [
        ("/api/v1/discover/movies/on_provider", "validation.provider"),
        ("/api/v1/discover/movies/genre", "validation.genre"),
        (
            "/api/v1/discover/movies/genre?genre=drama",
            "validation.genre",
        ),
        (
            "/api/v1/discover/movies/upcoming?sort=rating",
            "validation.sort",
        ),
        (
            "/api/v1/discover/movies/on_the_air",
            "discover.unknown_list",
        ),
        (
            "/api/v1/discover/movies/trending?provider=8",
            "validation.provider",
        ),
        (
            "/api/v1/discover/series/on_the_air?provider=8",
            "validation.provider",
        ),
    ] {
        let res = s.as_member(&s.mia, get(path)).await;
        assert_eq!(res.code(), code, "{path}: {}", res.raw);
    }
    let res = s
        .as_member(&s.mia, get("/api/v1/discover/movies/trending?provider=8"))
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
}

/// The detail view's facts (spec 0008 slice 2): certification, release
/// dates, runtime, cast and crew, providers of the category's regions,
/// and the season's episodes from their own endpoint.
#[tokio::test]
async fn details_carry_facts_cast_providers_and_episodes() {
    let s = setup().await;
    make_ready_with(
        &s,
        "movies",
        json!({ "providerRegion": "CH", "providers": [8], "releaseRegion": "CH" }),
    )
    .await;
    make_ready(&s, "series").await;
    let res = s
        .as_member(&s.mia, get(&format!("/api/v1/titles/movie/{MOVIE}")))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let d = &res.body;
    assert_eq!(
        d["certification"],
        json!({ "region": "CH", "rating": "12" })
    );
    assert_eq!(d["releaseDate"], "2010-07-15");
    assert_eq!(d["digitalReleaseDate"], "2010-11-20");
    assert_eq!(d["runtimeMinutes"], 148);
    assert_eq!(d["tagline"], "Your mind is the scene of the crime.");
    assert_eq!(d["cast"].as_array().unwrap().len(), 12);
    assert_eq!(d["cast"][0]["name"], "Leonardo DiCaprio");
    assert_eq!(
        d["crew"][0],
        json!({ "name": "Christopher Nolan", "role": "director" })
    );
    let providers: Vec<_> = d["watchProviders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["region"].as_str().unwrap(),
                p["kind"].as_str().unwrap(),
                p["name"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        providers,
        [("CH", "flatrate", "Netflix"), ("CH", "rent", "Apple TV")]
    );
    assert_eq!(d["attributions"][0]["source"], "JustWatch");
    assert_eq!(d["details"]["externalIds"]["imdb"], "tt1375666");
    assert_eq!(d["details"]["originalLanguage"], "en");
    assert_eq!(d["seriesStatus"], Value::Null);

    let series = s
        .as_member(&s.mia, get(&format!("/api/v1/titles/series/{SERIES}")))
        .await;
    assert_eq!(series.status, StatusCode::OK, "{}", series.raw);
    assert_eq!(series.body["seriesStatus"], "Ended");
    assert_eq!(series.body["networks"], json!(["HBO"]));
    assert_eq!(series.body["crew"][0]["role"], "creator");
    // No provider region in the series relevance: no providers shown.
    assert_eq!(series.body["watchProviders"], json!([]));
    assert_eq!(series.body["certification"], Value::Null);

    let season = s
        .as_member(
            &s.mia,
            get(&format!("/api/v1/titles/series/{SERIES}/seasons/1")),
        )
        .await;
    assert_eq!(season.status, StatusCode::OK, "{}", season.raw);
    assert_eq!(season.body["name"], "Season 1");
    let episodes: Vec<_> = season.body["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["number"].as_u64().unwrap())
        .collect();
    assert_eq!(episodes, [1, 2, 3]);
    assert_eq!(season.body["episodes"][0]["runtimeMinutes"], 62);
    // Unknown seasons (the fake has no season 7) and bad ids.
    let res = s
        .as_member(
            &s.mia,
            get(&format!("/api/v1/titles/series/{SERIES}/seasons/7")),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
    let res = s
        .as_member(&s.mia, get("/api/v1/titles/series/x/seasons/1"))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.raw);
    // Not entitled to series: absent.
    let lena = member(
        &s.env,
        "lena",
        json!([{ "category": "movies", "enabled": true, "requestMode": "approval" }]),
    )
    .await;
    let res = s
        .as_member(
            &lena,
            get(&format!("/api/v1/titles/series/{SERIES}/seasons/1")),
        )
        .await;
    assert_eq!(res.code(), "discover.unavailable", "{}", res.raw);
}

fn posters(res: &TestResponse) -> Vec<String> {
    res.body["posters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_owned())
        .collect()
}

// The fake TMDB server is kept alive as `_servers`; here it is counted.
#[allow(clippy::used_underscore_binding)]
async fn tmdb_requests(s: &Setup) -> usize {
    s._servers[0].received_requests().await.unwrap().len()
}

/// The login backdrop (spec 0008 slice 11): posters of the cached Trending
/// lists of switched-on movie and series categories, for anyone; never a
/// catalog call for a visitor, and its images only for those posters.
#[tokio::test]
async fn login_art_shows_cached_trending_posters_to_anyone() {
    let s = ready().await;
    let before = tmdb_requests(&s).await;
    // Nothing cached yet: no posters, and the visitor caused no request.
    let res = get("/api/v1/login-art").send(&s.env.app).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(posters(&res), Vec::<String>::new());
    assert_eq!(tmdb_requests(&s).await, before);

    // A member browsing fills the catalog cache.
    for list in ["movies", "series"] {
        let path = format!("/api/v1/discover/{list}/trending");
        let res = s.as_member(&s.mia, get(&path)).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    }
    let before = tmdb_requests(&s).await;
    let res = get("/api/v1/login-art").send(&s.env.app).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let found = posters(&res);
    assert!(found.len() >= 4, "{}", res.raw);
    assert!(found.len() <= 12, "{}", res.raw);
    // Movie first, then series, interleaved; file names for the proxy.
    assert_eq!(found[0], "ljsZTbVsrQSqZgWeep2B1QiDKuh.jpg");
    assert!(found.iter().all(|p| {
        !p.starts_with('/')
            && std::path::Path::new(p)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("jpg"))
    }));
    assert_eq!(tmdb_requests(&s).await, before);
    // Nothing about members or requests rides along.
    assert_eq!(res.body.as_object().unwrap().len(), 1, "{}", res.raw);

    // Images: only the posters shown, without a session. The test app has
    // no image proxy, so a poster that passes answers 503.
    let res = get(&format!("/api/v1/login-art/{}", found[0]))
        .send(&s.env.app)
        .await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE, "{}", res.raw);
    assert_eq!(res.code(), "images.unavailable");
    for file in ["kqjL17yufvn9OVLyXYpvtyrFfak.jpg", "..%2F..%2Fetc%2Fpasswd"] {
        let res = get(&format!("/api/v1/login-art/{file}"))
            .send(&s.env.app)
            .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{file}: {}", res.raw);
        assert_eq!(res.code(), "images.not_found");
    }

    // A switched-off category leaves the backdrop.
    let res = s
        .admin(patch(
            "/api/v1/categories/movies",
            json!({ "enabled": false }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = get("/api/v1/login-art").send(&s.env.app).await;
    let left = posters(&res);
    assert_ne!(left, Vec::<String>::new());
    assert!(!left.contains(&found[0]), "{}", res.raw);
}

/// A category without a saved relevance is not ready for Discover and stays
/// off the login wall, even when its Trending list is cached.
#[tokio::test]
async fn login_art_leaves_out_categories_without_a_relevance() {
    let s = setup().await;
    make_ready_with(&s, "series", json!({ "excludedGenres": [10767] })).await;
    let res = s
        .as_member(&s.mia, get("/api/v1/discover/series/trending"))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = get("/api/v1/login-art").send(&s.env.app).await;
    let found = posters(&res);
    assert_ne!(found, Vec::<String>::new(), "{}", res.raw);
    // Inception's poster (movies have no relevance yet) is not among them.
    assert!(!found.contains(&"ljsZTbVsrQSqZgWeep2B1QiDKuh.jpg".to_owned()));
}
