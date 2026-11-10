//! The TMDB image CDN (spec 0003 O4). Starr proxies every image so the
//! browser never contacts TMDB (lesson 31). The CDN needs no credential
//! and resolves `..` in paths (also `%2e%2e`), so sizes and file names are
//! validated strictly before anything is requested (capabilities.md, TMDB
//! "Images").

use std::time::Duration;

use starr_adapter_kit::{BinaryResponse, ServiceClient};
use starr_core::adapters::{AdapterError, RateLimit};
use url::Url;

/// Base URL of the CDN; images live at `{base}{size}/{file}`.
pub const CDN_BASE: &str = "https://image.tmdb.org/t/p/";

/// Every size `GET /3/configuration` lists for any image type (poster,
/// backdrop, logo, profile, still). The fixture contract test checks the
/// recorded configuration against this list.
pub const IMAGE_SIZES: &[&str] = &[
    "w45", "w92", "w154", "w185", "w300", "w342", "w500", "w780", "w1280", "h632", "original",
];

/// Largest image accepted from the CDN (`original` backdrops are a few MB).
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// The CDN limits simultaneous connections per IP (~20); stay well below
/// and reuse keep-alive connections (one client).
const CDN_RATE_LIMIT: RateLimit = RateLimit {
    requests: 20,
    per: Duration::from_secs(1),
};

/// Whether `size` is one of [`IMAGE_SIZES`].
#[must_use]
pub fn valid_size(size: &str) -> bool {
    IMAGE_SIZES.contains(&size)
}

/// Whether `file` is a TMDB image file name: 20–40 ASCII letters or digits
/// and `.jpg` or `.png`. Nothing else (no dots, slashes or encodings) can
/// pass, so a validated name cannot address another path.
#[must_use]
pub fn valid_file(file: &str) -> bool {
    let Some((stem, ext)) = file.split_once('.') else {
        return false;
    };
    (20..=40).contains(&stem.len())
        && stem.bytes().all(|b| b.is_ascii_alphanumeric())
        && matches!(ext, "jpg" | "png")
}

/// Client for the image CDN.
#[derive(Debug, Clone)]
pub struct ImageCdn {
    client: ServiceClient,
}

impl ImageCdn {
    /// The client for TMDB's CDN.
    pub fn new() -> Result<Self, AdapterError> {
        let base = Url::parse(CDN_BASE)
            .map_err(|err| AdapterError::Unknown(format!("invalid CDN base URL: {err}")))?;
        Self::with_base(base)
    }

    /// A client for another base URL (tests).
    pub fn with_base(base: Url) -> Result<Self, AdapterError> {
        Ok(Self {
            client: ServiceClient::public("tmdb-images", CDN_RATE_LIMIT, base)?,
        })
    }

    /// Fetch one image. `webp` asks the CDN for WebP (it serves it on
    /// `Accept: image/webp`). Invalid sizes or names are rejected without
    /// a request.
    pub async fn fetch(
        &self,
        size: &str,
        file: &str,
        webp: bool,
    ) -> Result<BinaryResponse, AdapterError> {
        if !valid_size(size) || !valid_file(file) {
            return Err(AdapterError::Rejected("invalid image path".to_owned()));
        }
        let accept = if webp {
            "image/webp,image/*;q=0.8"
        } else {
            "image/*"
        };
        self.client
            .get_bytes(&format!("{size}/{file}"), Some(accept), MAX_IMAGE_BYTES)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_file_names_pass() {
        assert!(valid_file("kqjL17yufvn9OVLyXYpvtyrFfak.jpg"));
        assert!(valid_file("wwemzKWzjKYJFfCeiB57q3r4Bcm.png"));
        assert!(valid_file("A1234567890123456789.jpg"));
    }

    #[test]
    fn traversal_and_odd_names_fail() {
        for bad in [
            "",
            "..",
            "../../etc/passwd",
            "..%2f..%2fetc.jpg",
            "%2e%2e/kqjL17yufvn9OVLyXYpvtyrFfak.jpg",
            "kqjL17yufvn9OVLyXYpvtyrFfak.jpg/..",
            "kqjL17yufvn9OVLyXYpvtyrFfak..jpg",
            "kqjL17yufvn9OVLyXYpvtyrFfak.jpeg",
            "kqjL17yufvn9OVLyXYpvtyrFfak.JPG",
            "kqjL17yufvn9OVLyXYpvtyrFfak.jpg.png",
            "kqjL17yufvn9OVLyXYpvtyrFfak.svg",
            "kqjL-7yufvn9OVLyXYpvtyrFfak.jpg",
            "short.jpg",
            "kqjL17yufvn9OVLyXYpvtyrFfakkqjL17yufvn9OVL.jpg",
            "kqjL17yufvn9OVLyXYpvtyrFfäk.jpg",
            "kqjL17yufvn9OVLyXYpvtyrFfak.jpg?x=1",
            "kqjL17yufvn9OVLyXYpvtyrFfak.jpg%00",
            "/kqjL17yufvn9OVLyXYpvtyrFfak.jpg",
        ] {
            assert!(!valid_file(bad), "{bad:?}");
        }
    }

    #[test]
    fn sizes_are_an_allowlist() {
        assert!(valid_size("w500"));
        assert!(valid_size("original"));
        for bad in ["", "w501", "..", "W500", "w500/..", "original/"] {
            assert!(!valid_size(bad), "{bad:?}");
        }
    }
}
