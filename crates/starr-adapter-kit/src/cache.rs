//! Response caching for adapters whose service publishes cache lifetimes
//! (TMDB). The kit computes how long a response may be kept; Starr provides
//! the storage ([`ResponseCache`], implemented over SQLite by the worker).

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use reqwest::header::{AGE, CACHE_CONTROL, HeaderMap};

/// A boxed future, so [`ResponseCache`] can be used as a trait object.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Storage for response bodies, keyed by request (path plus query). Best
/// effort: implementations log their own failures and answer `None` / do
/// nothing, so a broken cache only costs requests, never correctness.
pub trait ResponseCache: Send + Sync + fmt::Debug {
    /// The stored body for `key` if it has not expired.
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Vec<u8>>>;

    /// Store `body` for `key` for `ttl`.
    fn put<'a>(&'a self, key: &'a str, body: &'a [u8], ttl: Duration) -> BoxFuture<'a, ()>;
}

/// Bounds for cache lifetimes, set by the adapter from the service's
/// terms and budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePolicy {
    /// Kept at least this long, even with `max-age=0` or no header, so
    /// interactive browsing does not spend the service's budget.
    pub min_ttl: Duration,
    /// Never kept longer (e.g. TMDB terms: six months).
    pub max_ttl: Duration,
}

/// Remaining freshness of a response: `max-age` minus `Age`, clamped to
/// the policy. Without a usable `max-age` the minimum applies.
#[must_use]
pub fn cache_ttl(headers: &HeaderMap, policy: CachePolicy) -> Duration {
    let max_age = headers
        .get_all(CACHE_CONTROL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .find_map(|directive| {
            let (name, value) = directive.trim().split_once('=')?;
            if name.trim().eq_ignore_ascii_case("max-age") {
                value.trim().trim_matches('"').parse::<u64>().ok()
            } else {
                None
            }
        });
    let age = headers
        .get(AGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let fresh = Duration::from_secs(max_age.unwrap_or(0).saturating_sub(age));
    fresh.clamp(policy.min_ttl, policy.max_ttl.max(policy.min_ttl))
}

#[cfg(test)]
mod tests {
    use reqwest::header::HeaderValue;

    use super::*;

    const POLICY: CachePolicy = CachePolicy {
        min_ttl: Duration::from_mins(5),
        max_ttl: Duration::from_hours(183 * 24),
    };

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn max_age_minus_age() {
        let h = headers(&[("cache-control", "public, max-age=21600"), ("age", "600")]);
        assert_eq!(cache_ttl(&h, POLICY), Duration::from_mins(350));
    }

    #[test]
    fn short_zero_or_missing_lifetimes_get_the_minimum() {
        for h in [
            headers(&[("cache-control", "public, max-age=0")]),
            headers(&[("cache-control", "public, max-age=60")]),
            headers(&[("cache-control", "max-age=600"), ("age", "900")]),
            headers(&[("cache-control", "no-cache")]),
            headers(&[("cache-control", "max-age=abc")]),
            headers(&[]),
        ] {
            assert_eq!(cache_ttl(&h, POLICY), POLICY.min_ttl, "{h:?}");
        }
    }

    #[test]
    fn long_lifetimes_are_capped() {
        let h = headers(&[("cache-control", "public, MAX-AGE=31536000")]);
        assert_eq!(cache_ttl(&h, POLICY), POLICY.max_ttl);
    }

    #[test]
    fn directives_across_several_headers_are_read() {
        let h = headers(&[
            ("cache-control", "public"),
            ("cache-control", "max-age=\"900\""),
        ]);
        assert_eq!(cache_ttl(&h, POLICY), Duration::from_mins(15));
    }
}
