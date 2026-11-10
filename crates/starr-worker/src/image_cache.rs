//! Disk cache of proxied images (spec 0003 O4) under
//! `<data_dir>/cache/images/<source>/<size>/<stem>.<ext>`. The web process
//! reads and writes it; housekeeping in the worker keeps it under
//! the configured limit (`STARR_IMAGE_CACHE_MB`) by deleting the least recently used files
//! (modification time, refreshed on hits at most daily).
//!
//! The private category's images live in a tree of their own,
//! `<data_dir>/cache/images/adult/` ([`ImageCache::adult`], spec 0007 Q6):
//! the general cache never holds, counts or evicts them, and they have
//! their own limit (`STARR_ADULT_IMAGE_CACHE_MB`), go 30 days after their
//! last use ([`ADULT_UNUSED_FOR`]) and all at once when the category is
//! switched off ([`ImageCache::purge`]).
//!
//! Blocking file IO: call from `spawn_blocking`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Default size limit of the image cache (`STARR_IMAGE_CACHE_MB` = 1024).
pub const IMAGE_CACHE_DEFAULT_BYTES: u64 = 1024 * 1024 * 1024;
/// Default size limit of the adult image cache
/// (`STARR_ADULT_IMAGE_CACHE_MB` = 256, spec 0007 Q6).
pub const ADULT_IMAGE_CACHE_DEFAULT_BYTES: u64 = 256 * 1024 * 1024;
/// Adult images not served for this long are deleted (spec 0007 Q13).
pub const ADULT_UNUSED_FOR: Duration = Duration::from_hours(30 * 24);
/// Directory of the adult tree under the general cache root.
const ADULT_DIR: &str = "adult";
/// After eviction the cache holds at most this share of the limit, so it
/// does not evict again on the next run.
const EVICT_TO_PERCENT: u64 = 90;
/// A hit refreshes the file's modification time if it is older than this.
const TOUCH_AFTER: Duration = Duration::from_hours(24);
/// Leftover temporary files older than this are deleted.
const STALE_TEMP: Duration = Duration::from_hours(1);
const TEMP_MARKER: &str = ".tmp-";
/// Extension of the empty marker for "the source has no such image".
const MISSING_EXTENSION: &str = "missing";
/// Markers older than this are deleted by eviction (callers believe them
/// for a day at most).
const MISSING_KEPT: Duration = Duration::from_hours(48);

/// Stored image formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Webp,
    Jpeg,
    Png,
}

impl ImageFormat {
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Webp => "webp",
            Self::Jpeg => "jpg",
            Self::Png => "png",
        }
    }

    #[must_use]
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Webp => "image/webp",
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
        }
    }

    /// The format of a `Content-Type`; `None` for anything Starr does not
    /// serve as an image (never pass through e.g. `text/html` or SVG).
    #[must_use]
    pub fn from_content_type(value: &str) -> Option<Self> {
        let mime = value.split(';').next().unwrap_or("").trim();
        match mime.to_ascii_lowercase().as_str() {
            "image/webp" => Some(Self::Webp),
            "image/jpeg" | "image/jpg" => Some(Self::Jpeg),
            "image/png" => Some(Self::Png),
            _ => None,
        }
    }

    /// The format a file extension names.
    #[must_use]
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "webp" => Some(Self::Webp),
            "jpg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            _ => None,
        }
    }
}

/// What one eviction pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EvictReport {
    pub deleted: u64,
    pub remaining_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ImageCache {
    root: PathBuf,
    /// The adult tree: its file names are `StashDB` image ids, so its log
    /// lines never carry a path (spec 0007 "Server logs").
    private: bool,
}

/// Path components must be plain ASCII letters and digits; callers
/// validate first, this is the second line.
fn plain(component: &str) -> io::Result<&str> {
    if !component.is_empty() && component.bytes().all(|b| b.is_ascii_alphanumeric()) {
        Ok(component)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "image cache path component is not alphanumeric",
        ))
    }
}

impl ImageCache {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            private: false,
        }
    }

    /// The adult image cache next to this (general) one: its own tree,
    /// which the general cache leaves alone.
    #[must_use]
    pub fn adult(&self) -> Self {
        Self {
            root: self.root.join(ADULT_DIR),
            private: true,
        }
    }

    /// Delete every file of this cache (the adult cache when the category
    /// is switched off). Returns the number of files deleted.
    pub fn purge(&self) -> io::Result<u64> {
        let mut files = Vec::new();
        collect(&self.root, 0, &mut files)?;
        let deleted = files
            .iter()
            .filter(|(path, _, _)| self.remove(path))
            .count();
        Ok(deleted as u64)
    }

    /// Delete one file; trouble is logged (without the path for the adult
    /// tree) and counts as not deleted.
    fn remove(&self, path: &Path) -> bool {
        match fs::remove_file(path) {
            Ok(()) => true,
            Err(err) if err.kind() == io::ErrorKind::NotFound => false,
            Err(err) if self.private => {
                tracing::warn!(error = %err, "cannot delete cached image [adult]");
                false
            }
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "cannot delete cached image");
                false
            }
        }
    }

    /// A hit refreshes the file's modification time if it is older than
    /// [`TOUCH_AFTER`] (the "last used" eviction and retention read).
    fn touch_if_old(&self, path: &Path, meta: &fs::Metadata) {
        let old = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_some_and(|age| age > TOUCH_AFTER);
        if !old {
            return;
        }
        let touched = fs::File::options()
            .write(true)
            .open(path)
            .and_then(|f| f.set_modified(SystemTime::now()));
        match touched {
            Ok(()) => {}
            Err(err) if self.private => {
                tracing::debug!(error = %err, "cannot refresh cached image time [adult]");
            }
            Err(err) => {
                tracing::debug!(path = %path.display(), error = %err, "cannot refresh cached image time");
            }
        }
    }

    /// [`Self::evict`], deleting first every image not used for
    /// `unused_for` (modification time, refreshed by hits at most daily).
    pub fn evict_unused(&self, max_bytes: u64, unused_for: Duration) -> io::Result<EvictReport> {
        self.evict_inner(max_bytes, Some(unused_for))
    }

    fn dir(&self, source: &str, size: &str) -> io::Result<PathBuf> {
        Ok(self.root.join(plain(source)?).join(plain(size)?))
    }

    /// The first of `formats` cached for this image, preferred first.
    pub fn lookup(
        &self,
        source: &str,
        size: &str,
        stem: &str,
        formats: &[ImageFormat],
    ) -> io::Result<Option<(PathBuf, ImageFormat)>> {
        let dir = self.dir(source, size)?;
        let stem = plain(stem)?;
        for format in formats {
            let path = dir.join(format!("{stem}.{}", format.extension()));
            match fs::metadata(&path) {
                Ok(meta) if meta.is_file() => {
                    self.touch_if_old(&path, &meta);
                    return Ok(Some((path, *format)));
                }
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        Ok(None)
    }

    /// Store an image atomically (temporary file, then rename) and return
    /// its path. A "missing" marker for it goes.
    pub fn store(
        &self,
        source: &str,
        size: &str,
        stem: &str,
        format: ImageFormat,
        body: &[u8],
    ) -> io::Result<PathBuf> {
        let path = self.write(source, size, stem, format.extension(), body)?;
        let marker = self
            .dir(source, size)?
            .join(format!("{}.{MISSING_EXTENSION}", plain(stem)?));
        match fs::remove_file(marker) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
        Ok(path)
    }

    /// Remember that the source has no image under this name (a release
    /// group without a cover): an empty marker file, so the source is not
    /// asked again on every view.
    pub fn mark_missing(&self, source: &str, size: &str, stem: &str) -> io::Result<()> {
        self.write(source, size, stem, MISSING_EXTENSION, &[])
            .map(|_| ())
    }

    /// Whether a "missing" marker younger than `max_age` exists.
    pub fn is_missing(
        &self,
        source: &str,
        size: &str,
        stem: &str,
        max_age: Duration,
    ) -> io::Result<bool> {
        let path = self
            .dir(source, size)?
            .join(format!("{}.{MISSING_EXTENSION}", plain(stem)?));
        match fs::metadata(&path) {
            Ok(meta) => Ok(meta
                .modified()
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age < max_age)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        }
    }

    fn write(
        &self,
        source: &str,
        size: &str,
        stem: &str,
        extension: &str,
        body: &[u8],
    ) -> io::Result<PathBuf> {
        let dir = self.dir(source, size)?;
        let stem = plain(stem)?;
        let mut dirs = fs::DirBuilder::new();
        dirs.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut dirs, 0o700);
        dirs.create(&dir)?;
        let path = dir.join(format!("{stem}.{extension}"));
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let temp = dir.join(format!("{stem}{TEMP_MARKER}{}-{nanos}", std::process::id()));
        write_private(&temp, body)?;
        if let Err(err) = fs::rename(&temp, &path) {
            let _ = fs::remove_file(&temp);
            return Err(err);
        }
        Ok(path)
    }

    /// Delete stale temporary files, then the least recently used images
    /// until the cache is under `max_bytes` (with headroom).
    pub fn evict(&self, max_bytes: u64) -> io::Result<EvictReport> {
        self.evict_inner(max_bytes, None)
    }

    fn evict_inner(&self, max_bytes: u64, unused_for: Option<Duration>) -> io::Result<EvictReport> {
        let mut files = Vec::new();
        collect(&self.root, 0, &mut files)?;
        let now = SystemTime::now();
        let mut report = EvictReport::default();
        let mut images = Vec::with_capacity(files.len());
        for (path, len, modified) in files {
            let is_temp = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(TEMP_MARKER));
            if is_temp {
                if now.duration_since(modified).unwrap_or_default() > STALE_TEMP
                    && self.remove(&path)
                {
                    report.deleted += 1;
                }
                continue;
            }
            // Expired "no cover" markers weigh nothing, so size never
            // evicts them: they go once they no longer count.
            let is_marker = path.extension().is_some_and(|e| e == MISSING_EXTENSION);
            if is_marker {
                if now.duration_since(modified).unwrap_or_default() > MISSING_KEPT
                    && self.remove(&path)
                {
                    report.deleted += 1;
                }
                continue;
            }
            let unused = unused_for
                .is_some_and(|max| now.duration_since(modified).unwrap_or_default() > max);
            if unused {
                if self.remove(&path) {
                    report.deleted += 1;
                }
                continue;
            }
            images.push((path, len, modified));
        }
        let mut total: u64 = images.iter().map(|(_, len, _)| len).sum();
        if total > max_bytes {
            let target = max_bytes.saturating_mul(EVICT_TO_PERCENT) / 100;
            images.sort_by_key(|(_, _, modified)| *modified);
            for (path, len, _) in images {
                if total <= target {
                    break;
                }
                if self.remove(&path) {
                    total -= len;
                    report.deleted += 1;
                }
            }
        }
        report.remaining_bytes = total;
        Ok(report)
    }
}

/// Write `body` to a new file only its owner can read (0600).
fn write_private(path: &Path, body: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(body)
}

/// Files under `dir` (source/size/file: three levels), with size and
/// modification time. A missing root is an empty cache.
fn collect(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, u64, SystemTime)>) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    for entry in entries {
        // The web process renames and deletes files while this walks:
        // anything gone by now is simply skipped.
        let meta = match entry.and_then(|e| e.metadata().map(|m| (e, m))) {
            Ok(found) => found,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        };
        let (entry, meta) = meta;
        // The adult tree is a cache of its own (spec 0007 Q6).
        if depth == 0 && entry.file_name() == ADULT_DIR {
            continue;
        }
        if meta.is_dir() && depth < 2 {
            collect(&entry.path(), depth + 1, out)?;
        } else if meta.is_file() {
            out.push((
                entry.path(),
                meta.len(),
                meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "starr-image-cache-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn age(path: &Path, by: Duration) {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - by)
            .unwrap();
    }

    /// L4: cached images (the adult tree among them) are private to Starr's
    /// user, whatever the umask.
    #[cfg(unix)]
    #[test]
    fn stored_images_and_their_directories_are_private_to_the_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new();
        let cache = ImageCache::new(dir.0.clone()).adult();
        let path = cache
            .store("stashdb", "300", "abc", ImageFormat::Jpeg, b"jpeg")
            .unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        let mut parent = path.parent();
        while let Some(p) = parent.filter(|p| p.starts_with(&dir.0)) {
            assert_eq!(mode(p), 0o700, "{}", p.display());
            parent = p.parent();
        }
    }

    #[test]
    fn store_then_lookup_prefers_the_requested_format_order() {
        let dir = TempDir::new();
        let cache = ImageCache::new(dir.0.clone());
        assert_eq!(
            cache
                .lookup("tmdb", "w500", "abc", &[ImageFormat::Webp])
                .unwrap(),
            None
        );
        cache
            .store("tmdb", "w500", "abc", ImageFormat::Jpeg, b"jpeg")
            .unwrap();
        cache
            .store("tmdb", "w500", "abc", ImageFormat::Webp, b"webp")
            .unwrap();
        let (path, format) = cache
            .lookup(
                "tmdb",
                "w500",
                "abc",
                &[ImageFormat::Webp, ImageFormat::Jpeg],
            )
            .unwrap()
            .unwrap();
        assert_eq!(format, ImageFormat::Webp);
        assert_eq!(fs::read(path).unwrap(), b"webp");
        let (_, format) = cache
            .lookup("tmdb", "w500", "abc", &[ImageFormat::Jpeg])
            .unwrap()
            .unwrap();
        assert_eq!(format, ImageFormat::Jpeg);
    }

    #[test]
    fn missing_markers_expire_and_go_when_the_image_arrives() {
        let dir = TempDir::new();
        let cache = ImageCache::new(dir.0.clone());
        let day = Duration::from_hours(24);
        assert!(!cache.is_missing("caa", "250", "abc", day).unwrap());
        cache.mark_missing("caa", "250", "abc").unwrap();
        assert!(cache.is_missing("caa", "250", "abc", day).unwrap());
        assert!(!cache.is_missing("caa", "500", "abc", day).unwrap());
        assert!(
            !cache
                .is_missing("caa", "250", "abc", Duration::ZERO)
                .unwrap()
        );
        cache
            .store("caa", "250", "abc", ImageFormat::Jpeg, b"x")
            .unwrap();
        assert!(!cache.is_missing("caa", "250", "abc", day).unwrap());
        assert!(cache.mark_missing("caa", "250", "a-b").is_err());
        // Eviction removes markers long expired, never fresh ones.
        cache.mark_missing("caa", "500", "old").unwrap();
        cache.mark_missing("caa", "500", "new").unwrap();
        age(&dir.0.join("caa/500/old.missing"), Duration::from_hours(72));
        let report = cache.evict(u64::MAX).unwrap();
        assert_eq!(report.deleted, 1);
        assert!(!dir.0.join("caa/500/old.missing").exists());
        assert!(dir.0.join("caa/500/new.missing").exists());
    }

    #[test]
    fn non_alphanumeric_components_are_refused() {
        let dir = TempDir::new();
        let cache = ImageCache::new(dir.0.clone());
        for (size, stem) in [("..", "abc"), ("w500", "../x"), ("w500", ""), ("w5/0", "a")] {
            assert!(
                cache
                    .store("tmdb", size, stem, ImageFormat::Png, b"x")
                    .is_err()
            );
            assert!(
                cache
                    .lookup("tmdb", size, stem, &[ImageFormat::Png])
                    .is_err()
            );
        }
    }

    #[test]
    fn eviction_deletes_least_recently_used_first_and_stale_temp_files() {
        let dir = TempDir::new();
        let cache = ImageCache::new(dir.0.clone());
        let body = vec![0u8; 100];
        let old = cache
            .store("tmdb", "w92", "old", ImageFormat::Jpeg, &body)
            .unwrap();
        let mid = cache
            .store("tmdb", "w92", "mid", ImageFormat::Jpeg, &body)
            .unwrap();
        let new = cache
            .store("tmdb", "w500", "new", ImageFormat::Jpeg, &body)
            .unwrap();
        age(&old, Duration::from_hours(72));
        age(&mid, Duration::from_hours(48));
        let temp = old.with_file_name(format!("x{TEMP_MARKER}1-2"));
        fs::write(&temp, b"partial").unwrap();
        age(&temp, Duration::from_hours(2));

        // Under the limit: only the stale temp file goes.
        let report = cache.evict(1000).unwrap();
        assert_eq!(report.deleted, 1);
        assert!(!temp.exists());

        // 300 bytes, limit 150 → evict to 135: the two oldest go.
        let report = cache.evict(150).unwrap();
        assert_eq!(report.deleted, 2);
        assert_eq!(report.remaining_bytes, 100);
        assert!(!old.exists() && !mid.exists() && new.exists());
    }

    #[test]
    fn a_hit_refreshes_an_old_file_so_it_survives_eviction() {
        let dir = TempDir::new();
        let cache = ImageCache::new(dir.0.clone());
        let body = vec![0u8; 100];
        let a = cache
            .store("tmdb", "w92", "a", ImageFormat::Png, &body)
            .unwrap();
        let b = cache
            .store("tmdb", "w92", "b", ImageFormat::Png, &body)
            .unwrap();
        age(&a, Duration::from_hours(72));
        age(&b, Duration::from_hours(48));
        cache
            .lookup("tmdb", "w92", "a", &[ImageFormat::Png])
            .unwrap()
            .unwrap();
        cache.evict(150).unwrap();
        assert!(a.exists());
        assert!(!b.exists());
    }

    #[test]
    fn the_adult_tree_is_its_own_cache() {
        let dir = TempDir::new();
        let general = ImageCache::new(dir.0.clone());
        let adult = general.adult();
        let body = vec![0u8; 100];
        let poster = general
            .store("tmdb", "w92", "a", ImageFormat::Jpeg, &body)
            .unwrap();
        let scene = adult
            .store("stashdb", "300", "b", ImageFormat::Jpeg, &body)
            .unwrap();
        assert!(scene.starts_with(dir.0.join("adult")));
        // The general cache neither counts nor evicts adult files...
        let report = general.evict(50).unwrap();
        assert_eq!(report.deleted, 1);
        assert!(!poster.exists() && scene.exists());
        // ...nor purges them; the adult cache purges only its own.
        general
            .store("tmdb", "w92", "c", ImageFormat::Jpeg, &body)
            .unwrap();
        assert_eq!(general.purge().unwrap(), 1);
        assert!(scene.exists());
        assert_eq!(adult.purge().unwrap(), 1);
        assert!(!scene.exists());
        assert_eq!(adult.purge().unwrap(), 0);
    }

    #[test]
    fn unused_adult_images_go_after_thirty_days() {
        let dir = TempDir::new();
        let adult = ImageCache::new(dir.0.clone()).adult();
        let body = vec![0u8; 10];
        let old = adult
            .store("stashdb", "300", "old", ImageFormat::Jpeg, &body)
            .unwrap();
        let recent = adult
            .store("stashdb", "300", "recent", ImageFormat::Jpeg, &body)
            .unwrap();
        age(&old, ADULT_UNUSED_FOR + Duration::from_hours(1));
        age(&recent, Duration::from_hours(28 * 24));
        let report = adult.evict_unused(u64::MAX, ADULT_UNUSED_FOR).unwrap();
        assert_eq!(report.deleted, 1);
        assert!(!old.exists() && recent.exists());
        // The size limit applies as well.
        let report = adult.evict_unused(5, ADULT_UNUSED_FOR).unwrap();
        assert_eq!(report.deleted, 1);
        assert!(!recent.exists());
    }

    #[test]
    fn missing_root_is_an_empty_cache() {
        let dir = TempDir::new();
        let report = ImageCache::new(dir.0.join("nope")).evict(10).unwrap();
        assert_eq!(report, EvictReport::default());
    }

    #[test]
    fn content_types_map_to_formats() {
        assert_eq!(
            ImageFormat::from_content_type("image/webp"),
            Some(ImageFormat::Webp)
        );
        assert_eq!(
            ImageFormat::from_content_type("image/JPEG; charset=binary"),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(ImageFormat::from_content_type("image/svg+xml"), None);
        assert_eq!(ImageFormat::from_content_type("text/html"), None);
        assert_eq!(ImageFormat::from_extension("jpg"), Some(ImageFormat::Jpeg));
        assert_eq!(ImageFormat::from_extension("jpeg"), None);
    }
}
