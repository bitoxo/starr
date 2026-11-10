//! Whisparr v3 adapter (ADR 0009, ADR 0014): [`Probe`] and
//! [`Source`](starr_core::source::Source) for scenes keyed by their `StashDB`
//! UUID (Whisparr's `foreignId`, spec 0007 Q16).
//!
//! Whisparr holds private content: the client is built with
//! [`ClientOptions::private_content`] ([`client_options`]), so neither request
//! paths (they carry the `StashDB` UUID) nor Whisparr's own words reach logs
//! or error details, and this adapter's own errors never name a scene.
//!
//! Facts used here and their evidence: `docs/concept/capabilities.md`,
//! section "Radarr 6, Sonarr 4, Whisparr v3" (Whisparr source read at
//! `v3.1.0.2088` and `v3.1.0.2116`).

mod source;

use std::time::Duration;

use serde::Deserialize;
use starr_adapter_kit::{AuthScheme, ClientOptions, ServiceClient};
use starr_core::adapters::{
    AdapterDescriptor, AdapterError, Budgets, Capability, IdentityRule, Probe, ProbeReport,
    RateLimit, SecretClass, Version, VersionRange,
};

/// Whisparr authenticates API calls with the `X-Api-Key` header (Starr
/// never sends the key as a query parameter).
pub const AUTH: AuthScheme = AuthScheme::ApiKeyHeader("X-Api-Key");

/// Cheapest authenticated call that reports the version.
const STATUS_PATH: &str = "api/v3/system/status";

pub const DESCRIPTOR: AdapterDescriptor = AdapterDescriptor {
    type_id: "whisparr",
    display_name: "Whisparr",
    capabilities: &[Capability::Requests],
    // v3 only (branch `eros`, Radarr-based); the open 3.x range with its
    // accepted risk (spec 0007 Q2 B, ADR 0014). v2 is Sonarr-based and
    // read-only here.
    tested_versions: VersionRange {
        min: Version::new(3, 1, 0, 2088),
        max_exclusive: Version::new(4, 0, 0, 0),
    },
    budgets: Budgets {
        // Whisparr documents no rate limit; polite, as for Radarr.
        rate_limit: RateLimit {
            requests: 10,
            per: Duration::from_secs(1),
        },
        login_limit: None,
        // The unfiltered list computes statistics for the whole library;
        // a scene lookup asks Whisparr's metadata server.
        expensive_endpoints: &["api/v3/movie", "api/v3/lookup/scene"],
    },
    identity: IdentityRule::SingleUser,
    secret_class: SecretClass::ServiceCredential,
    lists: &[],
};

/// How a Whisparr client is built: the descriptor's budget, and private
/// content (spec 0007, server logs; ADR 0014).
#[must_use]
pub const fn client_options() -> ClientOptions {
    ClientOptions {
        private_content: true,
        ..ClientOptions::of(&DESCRIPTOR)
    }
}

/// The part of `GET /api/v3/system/status` the probe reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SystemStatus {
    app_name: String,
    version: String,
}

#[derive(Debug)]
pub struct Whisparr {
    client: ServiceClient,
}

impl Whisparr {
    #[must_use]
    pub fn new(client: ServiceClient) -> Self {
        Self { client }
    }
}

impl Probe for Whisparr {
    fn descriptor(&self) -> &'static AdapterDescriptor {
        &DESCRIPTOR
    }

    async fn probe(&self) -> Result<ProbeReport, AdapterError> {
        let status: SystemStatus = self.client.get_json(STATUS_PATH).await?;
        // Radarr, Sonarr & co. answer the same path; only Whisparr is
        // Whisparr. v2 says "Whisparr" too: the version range decides.
        if status.app_name != "Whisparr" {
            return Err(AdapterError::Unknown(format!(
                "the service at this URL is {:?}, not Whisparr",
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
