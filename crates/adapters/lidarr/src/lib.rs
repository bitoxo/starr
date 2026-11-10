//! Lidarr adapter (ADR 0009): [`Probe`] and [`Source`](starr_core::source::Source)
//! (artists keyed by `MusicBrainz` artist MBID, monitored per album, spec
//! 0006).
//!
//! Facts used here and their evidence: `docs/concept/capabilities.md`,
//! section "Lidarr".

mod source;

use std::time::Duration;

use serde::Deserialize;
use starr_adapter_kit::{AuthScheme, ServiceClient};
use starr_core::adapters::{
    AdapterDescriptor, AdapterError, Budgets, Capability, IdentityRule, Probe, ProbeReport,
    RateLimit, SecretClass, Version, VersionRange,
};

/// Lidarr authenticates API calls with the `X-Api-Key` header.
pub const AUTH: AuthScheme = AuthScheme::ApiKeyHeader("X-Api-Key");

/// Cheapest authenticated call that reports the version.
const STATUS_PATH: &str = "api/v1/system/status";

pub const DESCRIPTOR: AdapterDescriptor = AdapterDescriptor {
    type_id: "lidarr",
    display_name: "Lidarr",
    capabilities: &[Capability::Requests],
    // API v1 as read in the 3.x source (stable 3.1.6, pre-release 3.2.0);
    // 2.x and a future 4.x are read-only until fixtures prove them.
    tested_versions: VersionRange {
        min: Version::new(3, 0, 0, 0),
        max_exclusive: Version::new(4, 0, 0, 0),
    },
    budgets: Budgets {
        // Lidarr documents no rate limit; polite, as for Radarr and Sonarr.
        rate_limit: RateLimit {
            requests: 10,
            per: Duration::from_secs(1),
        },
        login_limit: None,
        // The unfiltered lists compute statistics for the whole library.
        expensive_endpoints: &["api/v1/artist", "api/v1/album"],
    },
    identity: IdentityRule::SingleUser,
    secret_class: SecretClass::ServiceCredential,
    lists: &[],
};

/// The part of `GET /api/v1/system/status` the probe reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SystemStatus {
    app_name: String,
    version: String,
}

#[derive(Debug)]
pub struct Lidarr {
    client: ServiceClient,
}

impl Lidarr {
    #[must_use]
    pub fn new(client: ServiceClient) -> Self {
        Self { client }
    }
}

impl Probe for Lidarr {
    fn descriptor(&self) -> &'static AdapterDescriptor {
        &DESCRIPTOR
    }

    async fn probe(&self) -> Result<ProbeReport, AdapterError> {
        let status: SystemStatus = self.client.get_json(STATUS_PATH).await?;
        // Radarr, Sonarr & co. answer the same path; only Lidarr is Lidarr.
        if status.app_name != "Lidarr" {
            return Err(AdapterError::Unknown(format!(
                "the service at this URL is {:?}, not Lidarr",
                self.client.redactor().redact(&status.app_name)
            )));
        }
        let version: Version = status.version.parse().map_err(|_| {
            AdapterError::Unknown(format!(
                "unparseable version {:?}",
                self.client.redactor().redact(&status.version)
            ))
        })?;
        DESCRIPTOR.check_version(version)?;
        Ok(ProbeReport { version })
    }
}
