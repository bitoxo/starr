//! Adapter contract (ADR 0009): what every service adapter declares and
//! guarantees. Pure types and traits; HTTP lives in `starr-adapter-kit`,
//! adapters in `crates/adapters/<name>`.

use std::cmp::Ordering;
use std::fmt;
use std::future::Future;
use std::str::FromStr;
use std::time::Duration;

/// What an adapter can do for Starr. Each capability maps to capability
/// traits the adapter implements; [`Probe`] is implied for every adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Capability {
    /// Catalog metadata (titles, posters, IDs).
    Metadata,
    /// Fulfils requests (adds titles to the service).
    Requests,
    /// Manages accounts in the service.
    Accounts,
    /// Libraries in the service: reading what they hold ([`Library`](crate::library::Library));
    /// granting access to them comes with accounts (phase 2).
    Libraries,
}

impl Capability {
    pub const ALL: [Self; 4] = [
        Self::Metadata,
        Self::Requests,
        Self::Accounts,
        Self::Libraries,
    ];

    /// Stable name used in the database and the API.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Requests => "requests",
            Self::Accounts => "accounts",
            Self::Libraries => "libraries",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

/// A service version as reported by the service, up to four numeric parts
/// (`5.14.0.9383`). Missing parts count as zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Version([u64; 4]);

impl Version {
    #[must_use]
    pub const fn new(major: u64, minor: u64, patch: u64, build: u64) -> Self {
        Self([major, minor, patch, build])
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [major, minor, patch, build] = self.0;
        write!(f, "{major}.{minor}.{patch}.{build}")
    }
}

/// The string is not a version of the form `1`, `1.2`, `1.2.3` or `1.2.3.4`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a version: {0:?}")]
pub struct VersionParseError(pub String);

impl FromStr for Version {
    type Err = VersionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || VersionParseError(s.to_owned());
        let mut parts = [0u64; 4];
        for (index, piece) in s.trim().split('.').enumerate() {
            let slot = parts.get_mut(index).ok_or_else(err)?;
            if piece.is_empty() || !piece.bytes().all(|b| b.is_ascii_digit()) {
                return Err(err());
            }
            *slot = piece.parse().map_err(|_| err())?;
        }
        Ok(Self(parts))
    }
}

/// Versions an adapter was tested against: `min` inclusive, `max` exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionRange {
    pub min: Version,
    pub max_exclusive: Version,
}

impl VersionRange {
    #[must_use]
    pub fn contains(&self, version: &Version) -> bool {
        &self.min <= version && version < &self.max_exclusive
    }
}

impl fmt::Display for VersionRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, ">={}, <{}", self.min, self.max_exclusive)
    }
}

/// At most `requests` calls per `per`, with bursts up to `requests`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    pub requests: u32,
    pub per: Duration,
}

/// What a service tolerates (lesson 11). All calls to one service instance
/// share one limiter built from `rate_limit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budgets {
    pub rate_limit: RateLimit,
    /// Limit on logins, if the service has one (health checks never log in).
    pub login_limit: Option<RateLimit>,
    /// Endpoints that are expensive for the service; callers use them sparingly.
    pub expensive_endpoints: &'static [&'static str],
}

/// How the service identifies the people using it (lesson 13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityRule {
    /// The service has one operator account and no per-person identity;
    /// Starr acts as that account and never provisions users.
    SingleUser,
    /// The service keys users by its own ID (Jellyfin, Emby); names may
    /// change, so Starr owns accounts by that remote ID (spec 0005 D4).
    RemoteId,
}

/// Which key seals an adapter's credential (ADR 0003, amendment 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretClass {
    /// Write-capable admin keys (Radarr, Sonarr, Jellyfin, Emby, …): sealed
    /// with the worker's `secret.key`, opened only by worker code.
    ServiceCredential,
    /// Read-only catalog tokens (TMDB, later `MusicBrainz`, `StashDB`, …):
    /// sealed with the web-held `web.key`, so interactive search and
    /// discover can call the catalog from the web process.
    CatalogReadToken,
}

impl SecretClass {
    /// Stable name used in the database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ServiceCredential => "service",
            Self::CatalogReadToken => "catalog",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::ServiceCredential, Self::CatalogReadToken]
            .into_iter()
            .find(|c| c.as_str() == s)
    }
}

/// Static description of an adapter type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterDescriptor {
    /// Stable id stored with each service instance, e.g. `radarr`.
    pub type_id: &'static str,
    /// Product name shown in the UI.
    pub display_name: &'static str,
    /// Declared capabilities. Phase 0 adapters may declare a capability they
    /// do not implement yet; Starr only calls traits the adapter implements.
    pub capabilities: &'static [Capability],
    pub tested_versions: VersionRange,
    pub budgets: Budgets,
    pub identity: IdentityRule,
    /// Which key seals the instance's credential.
    pub secret_class: SecretClass,
    /// Browse lists a catalog serves, in display order (ADR 0013); empty
    /// for adapters without [`Capability::Metadata`].
    pub lists: &'static [crate::catalog::ListKind],
}

impl AdapterDescriptor {
    /// Accept only versions inside the tested range (lesson 7).
    pub fn check_version(&self, found: Version) -> Result<(), AdapterError> {
        if self.tested_versions.contains(&found) {
            Ok(())
        } else {
            Err(AdapterError::VersionUnsupported {
                found,
                tested: self.tested_versions,
            })
        }
    }
}

/// The fixed error taxonomy (lesson 12). Retry, alerting and display decide
/// on the variant only; the strings are for logs and run reports and must
/// never contain secrets. Deliberately not `#[non_exhaustive]`, so every
/// match is exhaustive.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdapterError {
    /// Network trouble, timeout or server error; retrying later may help.
    #[error("transient failure: {0}")]
    Transient(String),
    /// The service asked us to slow down.
    #[error("throttled by the service (retry after {retry_after:?})")]
    Throttled { retry_after: Option<Duration> },
    /// The service rejected the credentials.
    #[error("credentials rejected by the service")]
    CredentialsInvalid,
    /// The addressed object does not exist in the service.
    #[error("not found: {0}")]
    NotFound(String),
    /// The service permanently refused the request.
    #[error("rejected by the service: {0}")]
    Rejected(String),
    /// The service runs a version outside the adapter's tested range.
    #[error("version {found} is outside the tested range {tested}")]
    VersionUnsupported {
        found: Version,
        tested: VersionRange,
    },
    /// Anything else, e.g. a response that does not parse.
    #[error("unexpected response: {0}")]
    Unknown(String),
}

impl AdapterError {
    /// Stable machine-readable code (stored as `last_error`, shown by the UI).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Transient(_) => "transient",
            Self::Throttled { .. } => "throttled",
            Self::CredentialsInvalid => "credentials_invalid",
            Self::NotFound(_) => "not_found",
            Self::Rejected(_) => "rejected",
            Self::VersionUnsupported { .. } => "version_unsupported",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// Result of a list operation. Decisions about absence ("not in the
/// service") are only allowed on a `complete` listing (lesson 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing<T> {
    pub items: Vec<T>,
    /// `true` only with proof that `items` is everything the service has.
    pub complete: bool,
}

/// What a successful probe proved: reachable, credentials accepted, and
/// this version (inside the tested range).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeReport {
    pub version: Version,
}

/// The cheapest call that proves reachability, credentials and version.
/// Never a login (lesson 11). Every adapter implements it.
pub trait Probe: Send + Sync {
    fn descriptor(&self) -> &'static AdapterDescriptor;

    fn probe(&self) -> impl Future<Output = Result<ProbeReport, AdapterError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    const RANGE: VersionRange = VersionRange {
        min: Version::new(5, 0, 0, 0),
        max_exclusive: Version::new(7, 0, 0, 0),
    };

    #[test]
    fn versions_parse_and_compare() {
        let v: Version = "5.14.0.9383".parse().unwrap();
        assert_eq!(v, Version::new(5, 14, 0, 9383));
        assert_eq!("6".parse::<Version>().unwrap(), Version::new(6, 0, 0, 0));
        assert!(Version::new(5, 2, 0, 0) < Version::new(5, 10, 0, 0));
        assert_eq!(v.to_string(), "5.14.0.9383");
        for bad in ["", "5.", "v5", "5.1.2.3.4", "5.x", "-1", "5 .1"] {
            assert!(bad.parse::<Version>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn range_is_min_inclusive_max_exclusive() {
        assert!(RANGE.contains(&Version::new(5, 0, 0, 0)));
        assert!(RANGE.contains(&Version::new(6, 99, 0, 0)));
        assert!(!RANGE.contains(&Version::new(7, 0, 0, 0)));
        assert!(!RANGE.contains(&Version::new(4, 7, 5, 7809)));
        assert_eq!(RANGE.to_string(), ">=5.0.0.0, <7.0.0.0");
    }

    #[test]
    fn descriptor_rejects_versions_outside_the_tested_range() {
        let descriptor = AdapterDescriptor {
            type_id: "x",
            display_name: "X",
            capabilities: &[],
            tested_versions: RANGE,
            budgets: Budgets {
                rate_limit: RateLimit {
                    requests: 1,
                    per: Duration::from_secs(1),
                },
                login_limit: None,
                expensive_endpoints: &[],
            },
            identity: IdentityRule::SingleUser,
            secret_class: SecretClass::ServiceCredential,
            lists: &[],
        };
        assert!(descriptor.check_version(Version::new(5, 1, 0, 0)).is_ok());
        let err = descriptor
            .check_version(Version::new(4, 0, 0, 0))
            .unwrap_err();
        assert_eq!(err.code(), "version_unsupported");
    }

    #[test]
    fn secret_class_names_round_trip() {
        for c in [
            SecretClass::ServiceCredential,
            SecretClass::CatalogReadToken,
        ] {
            assert_eq!(SecretClass::parse(c.as_str()), Some(c));
        }
        assert_eq!(SecretClass::parse("totp"), None);
    }

    #[test]
    fn capability_names_round_trip() {
        for c in Capability::ALL {
            assert_eq!(Capability::parse(c.as_str()), Some(c));
        }
        assert_eq!(Capability::parse("nope"), None);
    }
}
