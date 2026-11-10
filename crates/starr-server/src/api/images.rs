//! Image proxy: `/api/v1/images/{source}/{size}/{ref}` (spec 0003 O4,
//! lesson 31; generalised by ADR 0013). Sources: `tmdb` (`ref` is the file
//! name) and `caa`, the Cover Art Archive (`ref` is a release-group MBID,
//! spec 0006 Q8). The browser never contacts TMDB, the archive or
//! archive.org; Starr fetches each image once, keeps it on disk and serves
//! it with long browser caching. Size and reference are validated strictly
//! per source before anything touches the disk or the network: the TMDB
//! CDN resolves `..`. An album without a cover is remembered for a day, so
//! the archive is not asked on every view.
//!
//! The private category's source `stashdb` (`ref` is a `StashDB` image
//! id, spec 0007 Q6, ADR 0014) is different in every way: the privacy gate
//! comes first (closed: 404 before any cache or upstream is touched), the
//! files live in their own cache tree, and every answer is `private,
//! no-store` without `ETag`. Neither its references nor errors with them
//! are logged.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use starr_adapter_musicbrainz::covers::{self, CoverArchive};
use starr_adapter_tmdb::images::{ImageCdn, valid_file, valid_size};
use starr_core::adapters::AdapterError;
use starr_worker::image_cache::{ImageCache, ImageFormat};
use tokio::sync::Semaphore;

use super::error::{ApiError, ErrorBody};
use super::privacy;
use crate::AppState;
use crate::auth::{AnySession, Session};

/// Cache directory name of TMDB images.
const SOURCE: &str = "tmdb";
/// Cache directory name of Cover Art Archive covers.
const CAA: &str = "caa";
/// How long "this album has no cover" is believed (ADR 0013).
const MISSING_FOR: std::time::Duration = std::time::Duration::from_hours(24);
/// Concurrent CDN fetches; TMDB's CDN allows ~20 connections per IP.
const MAX_FETCHES: usize = 8;
/// Proxied images never change under their name (TMDB names are content
/// ids), but they are only served to logged-in accounts.
const BROWSER_CACHE: &str = "private, max-age=31536000, immutable";
/// Source name of adult images (`StashDB`, spec 0007 Q6).
const STASHDB: &str = "stashdb";
/// Adult images are never stored by a browser or an intermediary.
const ADULT_BROWSER_CACHE: &str = "private, no-store";

/// Upstream clients, disk cache and the fetch limit, shared by all
/// requests.
#[derive(Debug)]
pub struct ImageProxy {
    cdn: ImageCdn,
    covers: CoverArchive,
    cache: ImageCache,
    /// The adult image cache (its own tree, spec 0007 Q6).
    adult: ImageCache,
    fetches: Semaphore,
    /// Cover fetches wait for the archive's slow budget; they get their own
    /// permits so TMDB posters never queue behind them.
    cover_fetches: Semaphore,
    /// Adult fetches wait for `StashDB`'s budget; their own permits too.
    adult_fetches: Semaphore,
}

impl ImageProxy {
    #[must_use]
    pub fn new(cdn: ImageCdn, covers: CoverArchive, cache: ImageCache) -> Self {
        Self {
            cdn,
            covers,
            adult: cache.adult(),
            cache,
            fetches: Semaphore::new(MAX_FETCHES),
            cover_fetches: Semaphore::new(MAX_FETCHES),
            adult_fetches: Semaphore::new(MAX_FETCHES),
        }
    }

    /// Delete every cached adult image (the category was switched off).
    /// Blocking file IO.
    pub(crate) fn purge_adult(&self) -> std::io::Result<u64> {
        self.adult.purge()
    }
}

/// The cached image as a response (304 when `If-None-Match` matches, without
/// reading the file), or `None` on a miss.
async fn from_cache(
    proxy: &Arc<ImageProxy>,
    source: &'static str,
    size: &str,
    stem: &str,
    formats: &[ImageFormat],
    request: &HeaderMap,
) -> Result<Option<Response>, ApiError> {
    let cache = proxy.cache.clone();
    let (s, st, f) = (size.to_owned(), stem.to_owned(), formats.to_vec());
    let found = tokio::task::spawn_blocking(move || cache.lookup(source, &s, &st, &f))
        .await?
        .map_err(|err| {
            tracing::warn!(error = %err, "image cache lookup failed");
            ApiError::Internal
        })?;
    let Some((path, format)) = found else {
        return Ok(None);
    };
    let tag = etag(source, size, stem, format);
    if not_modified(request, &tag) {
        return Ok(Some(not_modified_response(&tag)));
    }
    match tokio::fs::read(&path).await {
        Ok(body) => Ok(Some(respond(body, format, &tag))),
        // Evicted between lookup and read: a miss.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => {
            tracing::warn!(error = %err, "reading a cached image failed");
            Err(ApiError::Internal)
        }
    }
}

fn respond(body: Vec<u8>, format: ImageFormat, etag: &str) -> Response {
    let mut res = Response::new(Body::from(body));
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.content_type()),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(BROWSER_CACHE),
    );
    headers.insert(header::VARY, HeaderValue::from_static("Accept"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }
    res
}

/// Strong `ETag` from the validated name and served format: a name always
/// stands for the same image.
fn etag(source: &str, size: &str, stem: &str, format: ImageFormat) -> String {
    format!("\"{source}-{size}-{stem}-{}\"", format.extension())
}

/// Whether `Accept` lists `image/webp` with a non-zero quality.
fn accepts_webp(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|range| {
            let mut parts = range.split(';').map(str::trim);
            let is_webp = parts
                .next()
                .is_some_and(|m| m.eq_ignore_ascii_case("image/webp"));
            let zero = parts.any(|p| {
                p.strip_prefix("q=")
                    .and_then(|q| q.parse::<f32>().ok())
                    .is_some_and(|q| q <= 0.0)
            });
            is_webp && !zero
        })
}

fn not_modified(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|tag| {
            let tag = tag.trim();
            tag == "*" || tag.trim_start_matches("W/") == etag
        })
}

fn not_modified_response(etag: &str) -> Response {
    let mut res = StatusCode::NOT_MODIFIED.into_response();
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(BROWSER_CACHE),
    );
    if let Ok(value) = HeaderValue::from_str(etag) {
        res.headers_mut().insert(header::ETAG, value);
    }
    res
}

/// An image of a catalog, proxied and cached by Starr; each source has its
/// own validator (ADR 0013). Prefers WebP when the browser accepts it.
#[utoipa::path(get, path = "/api/v1/images/{source}/{size}/{ref}", tag = "images",
    params(
        ("source" = String, Path, description = "Image source: `tmdb`, `caa` (Cover Art Archive) or `stashdb` (the private category: only through the privacy gate, never cached: `Cache-Control: private, no-store`, no `ETag`)"),
        ("size" = String, Path, description = "Image size of the source; TMDB: from its configuration, e.g. `w500`; CAA: `250`, `500` or `1200`; StashDB: `300`, `600` or `1280`"),
        ("ref" = String, Path, description = "Image reference of the source; TMDB: file name without the leading slash, e.g. `kqjL17yufvn9OVLyXYpvtyrFfak.jpg`; CAA: release-group MBID in lower case; StashDB: image UUID in lower case"),
    ),
    responses(
        (status = 200, description = "The image (`image/webp`, `image/jpeg` or `image/png`)", content_type = "image/*"),
        (status = 304, description = "Not modified (`If-None-Match`)"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 404, description = "`images.not_found`: unknown source, the source has no such image (an album without a cover), or a private image the session may not see", body = ErrorBody),
        (status = 422, description = "`validation.image_path`: size or reference not valid", body = ErrorBody),
        (status = 503, description = "`images.unavailable` (proxy not configured) or `images.upstream_unavailable` (source unreachable or asking Starr to slow down)", body = ErrorBody),
    ))]
pub async fn image(
    AnySession(session): AnySession,
    State(state): State<AppState>,
    Path((source, size, file)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    match source.as_str() {
        SOURCE => tmdb_image(&state, &size, &file, &headers).await,
        CAA => cover(&state, &size, &file, &headers).await,
        STASHDB => Ok(private_no_store(
            adult_image(&state, &session, &size, &file)
                .await
                .unwrap_or_else(IntoResponse::into_response),
        )),
        _ => Err(ApiError::NotFound("images.not_found")),
    }
}

/// An image of the adult catalog (spec 0007 Q6 A). The privacy gate comes
/// first: closed, the answer is 404 before any cache or upstream is
/// touched, as for an image that does not exist. Then the adult cache, then
/// `StashDB` (without the key, `StashDb::image`). Every answer (images and
/// errors) is `private, no-store` without `ETag`, and it never answers
/// 304.
async fn adult_image(
    state: &AppState,
    session: &Session,
    size: &str,
    reference: &str,
) -> Result<Response, ApiError> {
    if !privacy::adult_open(state, session).await? {
        return Err(ApiError::NotFound("images.not_found"));
    }
    if !starr_adapter_stashdb::valid_image_size(size)
        || !starr_adapter_stashdb::valid_image_ref(reference)
    {
        return Err(ApiError::Validation("validation.image_path"));
    }
    privacy::touch(state, session).await?;
    let proxy = state
        .images
        .clone()
        .ok_or(ApiError::Unavailable("images.unavailable"))?;
    // Cache names are letters and digits only.
    let stem = reference.replace('-', "");
    let formats = [ImageFormat::Jpeg, ImageFormat::Png, ImageFormat::Webp];
    if let Some(res) = from_adult_cache(&proxy, size, &stem, &formats).await? {
        return Ok(res);
    }
    let _permit = proxy
        .adult_fetches
        .acquire()
        .await
        .map_err(|_| ApiError::Internal)?;
    if let Some(res) = from_adult_cache(&proxy, size, &stem, &formats).await? {
        return Ok(res);
    }
    let catalog = super::private_catalog::private_catalog(state)
        .await?
        .ok_or(ApiError::NotFound("images.not_found"))?;
    let fetched = match catalog.image(size, reference).await {
        Ok(fetched) => fetched,
        Err(AdapterError::NotFound(_)) => return Err(ApiError::NotFound("images.not_found")),
        Err(err) => {
            // The code only: the reference is a private title's image.
            tracing::warn!(code = err.code(), "fetching an adult image failed");
            return Err(ApiError::Unavailable("images.upstream_unavailable"));
        }
    };
    let Some(format) = fetched
        .content_type
        .as_deref()
        .and_then(ImageFormat::from_content_type)
        .filter(|f| formats.contains(f))
    else {
        tracing::warn!("the adult catalog answered with an unexpected image type");
        return Err(ApiError::Unavailable("images.upstream_unavailable"));
    };
    let cache = proxy.adult.clone();
    let (s, body) = (size.to_owned(), fetched.body);
    let (body, stored) = tokio::task::spawn_blocking(move || {
        let stored = cache.store(STASHDB, &s, &stem, format, &body);
        (body, stored)
    })
    .await?;
    if let Err(err) = stored {
        tracing::warn!(error = %err, "storing an adult image in its cache failed");
    }
    Ok(respond_private(body, format))
}

/// A cached adult image, or `None` on a miss (no `ETag`, no 304).
async fn from_adult_cache(
    proxy: &Arc<ImageProxy>,
    size: &str,
    stem: &str,
    formats: &[ImageFormat],
) -> Result<Option<Response>, ApiError> {
    let cache = proxy.adult.clone();
    let (s, st, f) = (size.to_owned(), stem.to_owned(), formats.to_vec());
    let found = tokio::task::spawn_blocking(move || cache.lookup(STASHDB, &s, &st, &f))
        .await?
        .map_err(|err| {
            tracing::warn!(error = %err, "adult image cache lookup failed");
            ApiError::Internal
        })?;
    let Some((path, format)) = found else {
        return Ok(None);
    };
    match tokio::fs::read(&path).await {
        Ok(body) => Ok(Some(respond_private(body, format))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => {
            tracing::warn!(error = %err, "reading a cached adult image failed");
            Err(ApiError::Internal)
        }
    }
}

/// An adult image: never stored by a browser or an intermediary.
fn respond_private(body: Vec<u8>, format: ImageFormat) -> Response {
    let mut res = Response::new(Body::from(body));
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.content_type()),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    private_no_store(res)
}

/// Keep `res` out of every cache (browser disk cache, reverse proxies):
/// what adult responses carry (spec 0007 Q6 A).
fn private_no_store(mut res: Response) -> Response {
    let headers = res.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(ADULT_BROWSER_CACHE),
    );
    headers.remove(header::ETAG);
    res
}

/// A Cover Art Archive front cover: `mbid` is a release-group MBID.
async fn cover(
    state: &AppState,
    size: &str,
    mbid: &str,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    if !covers::valid_size(size) || !covers::valid_ref(mbid) {
        return Err(ApiError::Validation("validation.image_path"));
    }
    // Cache names are letters and digits only.
    let stem = mbid.replace('-', "");
    let proxy = state
        .images
        .clone()
        .ok_or(ApiError::Unavailable("images.unavailable"))?;
    // The archive serves JPEG thumbnails; PNG is kept as it came.
    let formats = [ImageFormat::Jpeg, ImageFormat::Png];
    if let Some(res) = from_cache(&proxy, CAA, size, &stem, &formats, headers).await? {
        return Ok(res);
    }
    if is_missing(&proxy, size, &stem).await? {
        return Err(ApiError::NotFound("images.not_found"));
    }
    let _permit = proxy
        .cover_fetches
        .acquire()
        .await
        .map_err(|_| ApiError::Internal)?;
    if let Some(res) = from_cache(&proxy, CAA, size, &stem, &formats, headers).await? {
        return Ok(res);
    }
    if is_missing(&proxy, size, &stem).await? {
        return Err(ApiError::NotFound("images.not_found"));
    }
    let fetched = match proxy.covers.front(size, mbid).await {
        Ok(fetched) => fetched,
        Err(AdapterError::NotFound(_)) => {
            let cache = proxy.cache.clone();
            let (s, st) = (size.to_owned(), stem.clone());
            let marked =
                tokio::task::spawn_blocking(move || cache.mark_missing(CAA, &s, &st)).await?;
            if let Err(err) = marked {
                tracing::warn!(error = %err, "remembering a missing cover failed");
            }
            return Err(ApiError::NotFound("images.not_found"));
        }
        Err(other) => {
            tracing::warn!(error = %other, %size, %mbid, "fetching a cover from the Cover Art Archive failed");
            return Err(ApiError::Unavailable("images.upstream_unavailable"));
        }
    };
    let Some(format) = fetched
        .content_type
        .as_deref()
        .and_then(ImageFormat::from_content_type)
        .filter(|f| formats.contains(f))
    else {
        tracing::warn!(content_type = ?fetched.content_type, %size, %mbid, "the Cover Art Archive answered with an unexpected type");
        return Err(ApiError::Unavailable("images.upstream_unavailable"));
    };
    let tag = etag(CAA, size, &stem, format);
    let cache = proxy.cache.clone();
    let (s, st, body) = (size.to_owned(), stem, fetched.body);
    let (body, stored) = tokio::task::spawn_blocking(move || {
        let stored = cache.store(CAA, &s, &st, format, &body);
        (body, stored)
    })
    .await?;
    if let Err(err) = stored {
        tracing::warn!(error = %err, "storing a cover in the cache failed");
    }
    Ok(respond(body, format, &tag))
}

/// Whether the archive said "no cover" for this album within a day.
async fn is_missing(proxy: &Arc<ImageProxy>, size: &str, stem: &str) -> Result<bool, ApiError> {
    let cache = proxy.cache.clone();
    let (s, st) = (size.to_owned(), stem.to_owned());
    tokio::task::spawn_blocking(move || cache.is_missing(CAA, &s, &st, MISSING_FOR))
        .await?
        .map_err(|err| {
            tracing::warn!(error = %err, "image cache lookup failed");
            ApiError::Internal
        })
}

/// A TMDB image: `file` is the TMDB file name.
pub(crate) async fn tmdb_image(
    state: &AppState,
    size: &str,
    file: &str,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let invalid = ApiError::Validation("validation.image_path");
    if !valid_size(size) || !valid_file(file) {
        return Err(invalid);
    }
    let (stem, ext) = file.split_once('.').ok_or(invalid)?;
    let original = ImageFormat::from_extension(ext).ok_or(invalid)?;
    let proxy = state
        .images
        .clone()
        .ok_or(ApiError::Unavailable("images.unavailable"))?;
    let webp = accepts_webp(headers);
    let formats = if webp {
        vec![ImageFormat::Webp, original]
    } else {
        vec![original]
    };

    if let Some(res) = from_cache(&proxy, SOURCE, size, stem, &formats, headers).await? {
        return Ok(res);
    }
    let _permit = proxy
        .fetches
        .acquire()
        .await
        .map_err(|_| ApiError::Internal)?;
    // Someone else may have fetched it while this request waited.
    if let Some(res) = from_cache(&proxy, SOURCE, size, stem, &formats, headers).await? {
        return Ok(res);
    }
    let fetched = proxy
        .cdn
        .fetch(size, file, webp)
        .await
        .map_err(|err| match err {
            AdapterError::NotFound(_) => ApiError::NotFound("images.not_found"),
            other => {
                tracing::warn!(error = %other, %size, %file, "fetching an image from TMDB failed");
                ApiError::Unavailable("images.upstream_unavailable")
            }
        })?;
    // Only images Starr knows are served; never pass through e.g. HTML.
    let Some(format) = fetched
        .content_type
        .as_deref()
        .and_then(ImageFormat::from_content_type)
    else {
        tracing::warn!(content_type = ?fetched.content_type, %size, %file, "TMDB answered with a non-image type");
        return Err(ApiError::Unavailable("images.upstream_unavailable"));
    };

    let tag = etag(SOURCE, size, stem, format);
    if !formats.contains(&format) {
        // A format this request's lookup would never find (e.g. JPEG for a
        // `.png` name): serve it, but do not fill the cache with it.
        tracing::debug!(%size, %file, ?format, "TMDB answered in an unexpected format; not cached");
        return Ok(respond(fetched.body, format, &tag));
    }
    let cache = proxy.cache.clone();
    let (s, st, body) = (size.to_owned(), stem.to_owned(), fetched.body);
    let (body, stored) = tokio::task::spawn_blocking(move || {
        let stored = cache.store(SOURCE, &s, &st, format, &body);
        (body, stored)
    })
    .await?;
    if let Err(err) = stored {
        // Serving works without the disk cache; it only costs a refetch.
        tracing::warn!(error = %err, "storing an image in the cache failed");
    }
    Ok(respond(body, format, &tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adult_answers_are_never_stored_and_carry_no_etag() {
        let tag = etag(STASHDB, "w300", "abc", ImageFormat::Jpeg);
        let res = private_no_store(respond(b"x".to_vec(), ImageFormat::Jpeg, &tag));
        assert_eq!(
            res.headers().get(header::CACHE_CONTROL).unwrap(),
            "private, no-store"
        );
        assert!(res.headers().get(header::ETAG).is_none());
        // Other sources keep their year of private caching.
        let res = respond(b"x".to_vec(), ImageFormat::Jpeg, &tag);
        assert_eq!(
            res.headers().get(header::CACHE_CONTROL).unwrap(),
            BROWSER_CACHE
        );
    }

    #[test]
    fn webp_needs_a_non_zero_quality() {
        let h = |v: &str| {
            let mut map = HeaderMap::new();
            map.insert(header::ACCEPT, HeaderValue::from_str(v).unwrap());
            map
        };
        assert!(accepts_webp(&h("image/avif,image/webp,*/*")));
        assert!(accepts_webp(&h("image/webp;q=0.8")));
        assert!(!accepts_webp(&h("image/webp;q=0, image/*")));
        assert!(!accepts_webp(&h("image/*")));
        assert!(!accepts_webp(&h("image/webpx")));
        assert!(!accepts_webp(&HeaderMap::new()));
    }

    #[test]
    fn if_none_match_accepts_lists_weak_tags_and_star() {
        let tag = etag("tmdb", "w500", "abc", ImageFormat::Webp);
        assert_eq!(tag, "\"tmdb-w500-abc-webp\"");
        let h = |v: &str| {
            let mut map = HeaderMap::new();
            map.insert(header::IF_NONE_MATCH, HeaderValue::from_str(v).unwrap());
            map
        };
        assert!(not_modified(&h("\"x\", \"tmdb-w500-abc-webp\""), &tag));
        assert!(not_modified(&h("W/\"tmdb-w500-abc-webp\""), &tag));
        assert!(not_modified(&h("*"), &tag));
        assert!(!not_modified(&h("\"tmdb-w500-abc-jpg\""), &tag));
        assert!(!not_modified(&HeaderMap::new(), &tag));
    }
}
