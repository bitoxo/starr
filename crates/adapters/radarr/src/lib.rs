//! Radarr adapter (ADR 0009): [`Probe`] and [`Source`](starr_core::source::Source)
//! (movies keyed by TMDB ID).
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

/// Radarr authenticates API calls with the `X-Api-Key` header.
pub const AUTH: AuthScheme = AuthScheme::ApiKeyHeader("X-Api-Key");

/// Cheapest authenticated call that reports the version.
const STATUS_PATH: &str = "api/v3/system/status";

pub const DESCRIPTOR: AdapterDescriptor = AdapterDescriptor {
    type_id: "radarr",
    display_name: "Radarr",
    capabilities: &[Capability::Requests],
    // v5 and v6 serve the same `/api/v3` surface; v7 is unreleased (develop).
    tested_versions: VersionRange {
        min: Version::new(5, 0, 0, 0),
        max_exclusive: Version::new(7, 0, 0, 0),
    },
    budgets: Budgets {
        // Radarr documents no rate limit; this keeps Starr a polite client
        // on small home servers.
        rate_limit: RateLimit {
            requests: 10,
            per: Duration::from_secs(1),
        },
        login_limit: None,
        expensive_endpoints: &["api/v3/movie", "api/v3/importlist/movie"],
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
pub struct Radarr {
    client: ServiceClient,
}

impl Radarr {
    #[must_use]
    pub fn new(client: ServiceClient) -> Self {
        Self { client }
    }
}

impl Probe for Radarr {
    fn descriptor(&self) -> &'static AdapterDescriptor {
        &DESCRIPTOR
    }

    async fn probe(&self) -> Result<ProbeReport, AdapterError> {
        let status: SystemStatus = self.client.get_json(STATUS_PATH).await?;
        // Sonarr, Lidarr & co. answer the same path; only Radarr is Radarr.
        if status.app_name != "Radarr" {
            return Err(AdapterError::Unknown(format!(
                "the service at this URL is {:?}, not Radarr",
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
