//! The login page's backdrop (spec 0008 slice 11): poster art from the
//! cached Trending lists of the movie and series categories, for visitors
//! who are not signed in.
//!
//! Privacy and cost rules: only the movie and series categories are ever
//! looked at (never the private category, never music); only categories
//! that are switched on with a saved relevance, filtered by it as Discover
//! is; only what the catalog cache already holds, so a visitor never causes
//! a catalog call; and only file names, nothing about members or requests.
//! The images come through [`login_art_image`], which serves exactly the
//! posters listed, so the image proxy stays closed to visitors.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use serde::Serialize;
use starr_core::catalog::MediaKind;
use starr_core::members::Category;
use starr_worker::catalog::{AnyCatalog, CatalogOpenError};
use utoipa::ToSchema;

use super::catalog::file_name;
use super::error::{ApiError, ErrorBody};
use super::images::tmdb_image;
use crate::AppState;

/// Posters on the login page at most.
const POSTERS: usize = 12;
/// The poster size served (the login wall shows small covers).
const SIZE: &str = "w342";
/// As Discover (D5: English in v1).
const LANGUAGE: &str = "en-US";
/// The categories the backdrop may show: never the private category.
const CATEGORIES: [(Category, MediaKind); 2] = [
    (Category::Movies, MediaKind::Movie),
    (Category::Series, MediaKind::Series),
];

/// How long serving images trusts the posters last listed.
const MEMO_TTL: Duration = Duration::from_secs(60);
/// A file missing from a list younger than this is refused without
/// rebuilding the list, so unknown names cost nothing.
const MEMO_MIN_AGE: Duration = Duration::from_secs(10);

/// The posters last listed and when (in [`AppState`]): serving the wall's
/// images checks against it instead of rebuilding the list per image.
#[derive(Debug, Default)]
pub struct LoginArtMemo(Mutex<Option<(Instant, Vec<String>)>>);

impl LoginArtMemo {
    /// Whether `file` is listed: `Some` while the memo answers, `None` when
    /// the list must be rebuilt.
    fn lookup(&self, file: &str) -> Option<bool> {
        let memo = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let (at, list) = memo.as_ref()?;
        let age = at.elapsed();
        if age >= MEMO_TTL {
            return None;
        }
        if list.iter().any(|p| p == file) {
            return Some(true);
        }
        (age < MEMO_MIN_AGE).then_some(false)
    }

    fn store(&self, list: &[String]) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((Instant::now(), list.to_vec()));
    }
}

/// Poster art for the login page.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoginArt {
    /// TMDB poster file names, served by `GET /api/v1/login-art/{file}`;
    /// empty while nothing is cached (the page then shows no art).
    posters: Vec<String>,
}

/// Poster art for the login page: up to 12 posters of the cached Trending
/// lists of the switched-on movie and series categories, interleaved.
/// Public; reads the catalog cache only.
#[utoipa::path(get, path = "/api/v1/login-art", tag = "auth",
    responses(
        (status = 200, body = LoginArt),
        (status = 500, body = ErrorBody),
    ))]
pub async fn login_art(State(state): State<AppState>) -> Result<Json<LoginArt>, ApiError> {
    let posters = posters(&state).await?;
    state.login_art.store(&posters);
    Ok(Json(LoginArt { posters }))
}

/// One poster of [`login_art`] (`w342`, WebP when accepted). Public; any
/// file not listed there right now is not found.
#[utoipa::path(get, path = "/api/v1/login-art/{file}", tag = "auth",
    params(("file" = String, Path, description = "A poster file name from `GET /api/v1/login-art`")),
    responses(
        (status = 200, description = "The image (`image/webp` or `image/jpeg`)", content_type = "image/*"),
        (status = 304, description = "Not modified (`If-None-Match`)"),
        (status = 404, description = "`images.not_found`: not one of the posters shown, or TMDB has no such image", body = ErrorBody),
        (status = 503, description = "`images.unavailable`, `images.upstream_unavailable`", body = ErrorBody),
    ))]
pub async fn login_art_image(
    State(state): State<AppState>,
    Path(file): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let not_found = ApiError::NotFound("images.not_found");
    if !starr_adapter_tmdb::images::valid_file(&file) {
        return Err(not_found);
    }
    let listed = if let Some(listed) = state.login_art.lookup(&file) {
        listed
    } else {
        let posters = posters(&state).await?;
        state.login_art.store(&posters);
        posters.contains(&file)
    };
    if !listed {
        return Err(not_found);
    }
    tmdb_image(&state, SIZE, &file, &headers).await
}

/// The posters of each category's cached Trending list, interleaved.
async fn posters(state: &AppState) -> Result<Vec<String>, ApiError> {
    let mut lanes = Vec::new();
    for (category, kind) in CATEGORIES {
        if !state.db.category_state(category).await?.enabled {
            continue;
        }
        let Some(role) = state.db.default_catalog(category).await? else {
            continue;
        };
        let Some(relevance) = role.relevance()? else {
            continue;
        };
        // Only TMDB lists carry posters for this; a catalog that cannot be
        // opened leaves its category out, as nothing is cached for it then.
        let tmdb = match state
            .catalog
            .catalog(&state.db, role.service_instance_id)
            .await
        {
            Ok(AnyCatalog::Tmdb(tmdb)) => tmdb,
            Err(CatalogOpenError::Db(err)) => return Err(err.into()),
            Ok(_) | Err(_) => continue,
        };
        let Some(page) = tmdb.cached_trending(kind, LANGUAGE, &relevance).await else {
            continue;
        };
        lanes.push(
            page.items
                .into_iter()
                .filter_map(|e| file_name(e.poster))
                .collect::<Vec<_>>(),
        );
    }
    Ok(interleave(lanes, POSTERS))
}

/// Every lane's first, then every lane's second, …; no file twice, at
/// most `max`.
fn interleave(lanes: Vec<Vec<String>>, max: usize) -> Vec<String> {
    let mut lanes: Vec<_> = lanes.into_iter().map(Vec::into_iter).collect();
    let mut out: Vec<String> = Vec::new();
    loop {
        let mut any = false;
        for lane in &mut lanes {
            let Some(file) = lane.next() else { continue };
            any = true;
            if out.len() < max && !out.contains(&file) {
                out.push(file);
            }
        }
        if !any || out.len() >= max {
            return out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(files: &[&str]) -> Vec<String> {
        files.iter().map(|f| (*f).to_owned()).collect()
    }

    #[test]
    fn lanes_interleave_without_repeats_up_to_the_cap() {
        let out = interleave(vec![lane(&["a", "b", "c"]), lane(&["x", "a", "y"])], 12);
        assert_eq!(out, ["a", "x", "b", "c", "y"]);
        let out = interleave(vec![lane(&["a", "b", "c"]), lane(&["x", "y"])], 3);
        assert_eq!(out, ["a", "x", "b"]);
        assert_eq!(interleave(Vec::new(), 12), Vec::<String>::new());
    }

    #[test]
    fn the_memo_answers_while_fresh_and_refuses_unknown_files_cheaply() {
        let memo = LoginArtMemo::default();
        assert_eq!(memo.lookup("a.jpg"), None);
        memo.store(&lane(&["a.jpg"]));
        assert_eq!(memo.lookup("a.jpg"), Some(true));
        assert_eq!(memo.lookup("b.jpg"), Some(false));
        let old = Instant::now().checked_sub(MEMO_MIN_AGE).unwrap();
        *memo.0.lock().unwrap() = Some((old, lane(&["a.jpg"])));
        assert_eq!(memo.lookup("a.jpg"), Some(true));
        assert_eq!(memo.lookup("b.jpg"), None);
        let expired = Instant::now().checked_sub(MEMO_TTL).unwrap();
        *memo.0.lock().unwrap() = Some((expired, lane(&["a.jpg"])));
        assert_eq!(memo.lookup("a.jpg"), None);
    }

    #[test]
    fn the_private_category_is_never_looked_at() {
        assert!(
            CATEGORIES
                .iter()
                .all(|(c, _)| !starr_core::categories::is_private(*c))
        );
    }
}
