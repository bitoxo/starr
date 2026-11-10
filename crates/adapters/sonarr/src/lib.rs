//! Sonarr adapter (ADR 0009): [`Probe`] and [`Source`](starr_core::source::Source)
//! (series keyed by TVDB ID).
//!
//! Facts used here and their evidence: `docs/concept/capabilities.md`,
//! section "Radarr 6, Sonarr 4, Whisparr v3".

mod source;

use std::time::Duration;

use serde::Deserialize;
use starr_adapter_kit::{AuthScheme, ServiceClient};
use starr_core::adapters::{
    AdapterDescriptor, AdapterError, Budgets, Capability, IdentityRule, Probe, ProbeReport,
    RateLimit, SecretClass, Version, VersionRange,
};

/// Sonarr authenticates API calls with the `X-Api-Key` header.
pub const AUTH: AuthScheme = AuthScheme::ApiKeyHeader("X-Api-Key");

/// Cheapest authenticated call that reports the version.
const STATUS_PATH: &str = "api/v3/system/status";

pub const DESCRIPTOR: AdapterDescriptor = AdapterDescriptor {
    type_id: "sonarr",
    display_name: "Sonarr",
    capabilities: &[Capability::Requests],
    // v4 serves `/api/v3`. `addOptions.monitor: "skip"` (use the seasons'
    // monitored flags as sent) exists from 4.0.4; v5 is pre-release only.
    tested_versions: VersionRange {
        min: Version::new(4, 0, 4, 0),
        max_exclusive: Version::new(5, 0, 0, 0),
    },
    budgets: Budgets {
        // Sonarr documents no rate limit; this keeps Starr a polite client
        // on small home servers.
        rate_limit: RateLimit {
            requests: 10,
            per: Duration::from_secs(1),
        },
        login_limit: None,
        // The unfiltered series list computes statistics for every series.
        expensive_endpoints: &["api/v3/series"],
    },
    identity: IdentityRule::SingleUser,
    secret_class: SecretClass::ServiceCredential,
    lists: &[],
};

/// The part of `GET /api/v3/system/status` the probe reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SystemStatus {
    app_name: String,
    version: String,
}

#[derive(Debug)]
pub struct Sonarr {
    client: ServiceClient,
}

impl Sonarr {
    #[must_use]
    pub fn new(client: ServiceClient) -> Self {
        Self { client }
    }
}

impl Probe for Sonarr {
    fn descriptor(&self) -> &'static AdapterDescriptor {
        &DESCRIPTOR
    }

    async fn probe(&self) -> Result<ProbeReport, AdapterError> {
        let status: SystemStatus = self.client.get_json(STATUS_PATH).await?;
        // Radarr, Lidarr & co. answer the same path; only Sonarr is Sonarr.
        if status.app_name != "Sonarr" {
            return Err(AdapterError::Unknown(format!(
                "the service at this URL is {:?}, not Sonarr",
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
