//! The Cover Art Archive (spec 0006 Q8, ADR 0013): album covers by
//! release-group MBID for Starr's image proxy, so the browser never
//! contacts the archive. `GET /release-group/{mbid}/front-{size}` answers
//! 307 to `archive.org`, which answers 302 to one of its storage hosts
//! (each hop followed only to `archive.org` or `*.archive.org` over https,
//! at most [`starr_adapter_kit::MAX_REDIRECTS`]), or 404 when no front
//! image was chosen. Size and MBID are validated
//! strictly before anything is requested (capabilities.md, Cover Art
//! Archive).

use std::time::Duration;

use starr_adapter_kit::{BinaryResponse, ClientOptions, ServiceClient};
use starr_core::adapters::{AdapterError, RateLimit};

/// Longest a cover request waits for its turn in the budget.
const MAX_WAIT: Duration = Duration::from_secs(15);
use url::Url;
use uuid::Uuid;

/// Base URL of the archive.
pub const CAA_BASE: &str = "https://coverartarchive.org/";

/// The thumbnail sizes the archive serves for a front image.
pub const COVER_SIZES: &[&str] = &["250", "500", "1200"];

/// Largest cover accepted (a 1200 px JPEG is well below).
pub const MAX_COVER_BYTES: usize = 10 * 1024 * 1024;

/// The archive documents no rate limit but lists 503 for exceeding one;
/// Starr stays at a few requests per second. Only the request to the
/// archive counts: the redirects to `archive.org` and its storage host go
/// to other hosts and take nothing from this budget.
pub const CAA_RATE_LIMIT: RateLimit = RateLimit {
    requests: 4,
    per: Duration::from_secs(1),
};

/// Whether `size` is one of [`COVER_SIZES`].
#[must_use]
pub fn valid_size(size: &str) -> bool {
    COVER_SIZES.contains(&size)
}

/// Whether `reference` is a release-group MBID in canonical spelling
/// (lower-case, hyphenated). Nothing else can pass, so a validated
/// reference cannot address another path.
#[must_use]
pub fn valid_ref(reference: &str) -> bool {
    Uuid::parse_str(reference).is_ok_and(|id| id.hyphenated().to_string() == reference)
}

/// Whether the archive may send Starr to `url`: https on `archive.org` or
/// one of its hosts.
#[must_use]
pub fn archive_host(url: &Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|h| h == "archive.org" || h.ends_with(".archive.org"))
        && url.username().is_empty()
        && url.password().is_none()
}

/// Client for the Cover Art Archive.
#[derive(Debug, Clone)]
pub struct CoverArchive {
    client: ServiceClient,
    base: Url,
}

impl CoverArchive {
    /// The client for the archive.
    pub fn new() -> Result<Self, AdapterError> {
        let base = Url::parse(CAA_BASE)
            .map_err(|err| AdapterError::Unknown(format!("invalid archive base URL: {err}")))?;
        Self::with_base(base)
    }

    /// A client for another base URL (tests, development fakes). Redirects
    /// to the base's own origin are followed too.
    pub fn with_base(base: Url) -> Result<Self, AdapterError> {
        Ok(Self {
            client: ServiceClient::public_with(
                "coverartarchive",
                ClientOptions {
                    rate_limit: CAA_RATE_LIMIT,
                    unavailable_is_throttle: true,
                    max_wait: Some(MAX_WAIT),
                    private_content: false,
                },
                base.clone(),
            )?,
            base,
        })
    }

    /// The front cover of release group `mbid` in `size`. No front image
    /// is [`AdapterError::NotFound`]; invalid sizes or references are
    /// rejected without a request.
    pub async fn front(&self, size: &str, mbid: &str) -> Result<BinaryResponse, AdapterError> {
        if !valid_size(size) || !valid_ref(mbid) {
            return Err(AdapterError::Rejected("invalid cover path".to_owned()));
        }
        let origin = self.base.origin();
        self.client
            .get_bytes_redirected(
                &format!("release-group/{mbid}/front-{size}"),
                Some("image/*"),
                MAX_COVER_BYTES,
                |url| archive_host(url) || url.origin() == origin,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_are_canonical_mbids_only() {
        assert!(valid_ref("a74b1b7f-71a5-4011-9441-d0b5e4122711"));
        for bad in [
            "",
            "A74B1B7F-71A5-4011-9441-D0B5E4122711",
            "a74b1b7f71a540119441d0b5e4122711",
            "{a74b1b7f-71a5-4011-9441-d0b5e4122711}",
            "../a74b1b7f-71a5-4011-9441-d0b5e4122711",
            "a74b1b7f-71a5-4011-9441-d0b5e4122711/..",
            "a74b1b7f-71a5-4011-9441-d0b5e4122711%00",
            "urn:uuid:a74b1b7f-71a5-4011-9441-d0b5e4122711",
        ] {
            assert!(!valid_ref(bad), "{bad:?}");
        }
    }

    #[test]
    fn sizes_are_an_allowlist() {
        assert!(valid_size("250"));
        assert!(valid_size("1200"));
        for bad in ["", "small", "251", "w500", "250/..", "original"] {
            assert!(!valid_size(bad), "{bad:?}");
        }
    }

    #[test]
    fn only_https_archive_hosts_are_followed() {
        let ok = |u: &str| archive_host(&Url::parse(u).unwrap());
        assert!(ok("https://archive.org/download/x/y.jpg"));
        assert!(ok("https://ia800505.us.archive.org/1/items/x/y.jpg"));
        assert!(!ok("http://ia800505.us.archive.org/x.jpg"));
        assert!(!ok("https://archive.org.evil.example/x.jpg"));
        assert!(!ok("https://evilarchive.org/x.jpg"));
        assert!(!ok("https://user:pw@archive.org/x.jpg"));
    }
}
