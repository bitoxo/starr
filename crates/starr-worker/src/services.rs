//! `services.check`: open the credential, run the adapter's probe, store
//! version, capabilities and error code, report per instance.

use secrecy::SecretString;
use starr_adapter_kit::{ClientPool, Endpoint, ServiceClient};
use starr_core::adapters::{AdapterDescriptor, AdapterError, Capability, SecretClass};
use starr_core::jobs::{Expectation, ItemOutcome, Report, ReportItem};
use starr_db::services::{CheckOutcome, ServiceConnection};
use url::Url;

use crate::catalog::{CODE_CATALOG_KEY_UNAVAILABLE, CatalogAccess};
use crate::secrets::CredentialKey;
use crate::{JobContext, JobError, adapters};

/// Error codes of failures on Starr's side (next to the adapter taxonomy).
pub const CODE_CREDENTIALS_UNREADABLE: &str = "credentials_unreadable";
pub const CODE_ADAPTER_UNKNOWN: &str = "adapter_unknown";

/// A failed check: taxonomy code, operator detail, version if one was seen.
pub(crate) struct Failure {
    pub(crate) code: &'static str,
    pub(crate) detail: String,
    version: Option<String>,
}

impl Failure {
    /// A failure on Starr's side (no service version involved).
    pub(crate) fn starr(code: &'static str, detail: String) -> Self {
        Self {
            code,
            detail,
            version: None,
        }
    }
}

impl From<AdapterError> for Failure {
    fn from(err: AdapterError) -> Self {
        let version = match &err {
            AdapterError::VersionUnsupported { found, .. } => Some(found.to_string()),
            _ => None,
        };
        Self {
            code: err.code(),
            detail: err.to_string(),
            version,
        }
    }
}

struct Success {
    version: String,
    capabilities: &'static [Capability],
}

/// The keys and client pools a job opens instances with.
#[derive(Clone, Copy)]
pub(crate) struct Openers<'a> {
    pub(crate) credentials: &'a CredentialKey,
    pub(crate) clients: &'a ClientPool,
    pub(crate) catalog: Option<&'a CatalogAccess>,
}

pub(crate) async fn check(ctx: &JobContext, openers: Openers<'_>) -> Result<Report, JobError> {
    let ids = match ctx.payload.get("instanceId") {
        Some(id) => vec![
            id.as_i64()
                .ok_or_else(|| JobError::Failed("instanceId must be an integer".to_owned()))?,
        ],
        None => ctx.db.service_ids().await?,
    };

    let mut items = Vec::with_capacity(ids.len());
    let (mut ok, mut failed, mut skipped) = (0u64, 0u64, 0u64);
    for id in ids {
        if ctx.cancel.is_cancelled() {
            return Err(JobError::Cancelled);
        }
        let item = check_one(ctx, openers, id).await?;
        match item.outcome {
            ItemOutcome::Succeeded => ok += 1,
            ItemOutcome::Failed => failed += 1,
            ItemOutcome::Skipped => skipped += 1,
        }
        items.push(item);
    }

    let total = ok + failed + skipped;
    let expectation = if total == skipped {
        Expectation::NotApplicable
    } else if failed == 0 {
        Expectation::Met(format!("{ok} of {} instances reachable", ok + failed))
    } else {
        Expectation::Unmet(format!(
            "{failed} of {} instances failed the check",
            ok + failed
        ))
    };
    let mut report = Report::new(total, ok, failed, skipped, expectation)?
        .with_counter("checked", ok + failed)
        .with_counter("failed", failed);
    for item in items {
        report = report.with_item(item);
    }
    Ok(report)
}

/// How often one instance is re-probed when its URL or credential changed
/// while a probe ran. A pending check job for the instance is deduplicated
/// against this running one, so the re-check has to happen here.
const MAX_PROBES_PER_INSTANCE: usize = 3;

async fn check_one(
    ctx: &JobContext,
    openers: Openers<'_>,
    id: i64,
) -> Result<ReportItem, JobError> {
    let skipped = |target: String, detail: &str| ReportItem {
        target,
        outcome: ItemOutcome::Skipped,
        code: None,
        detail: Some(detail.to_owned()),
    };
    let mut target = format!("service {id}");
    for _ in 0..MAX_PROBES_PER_INSTANCE {
        let Some(conn) = ctx.db.service_connection(id).await? else {
            openers.clients.remove(id);
            return Ok(skipped(target, "removed before the check ran"));
        };
        target = format!("{} ({})", conn.name, conn.adapter_type);
        let result = tokio::select! {
            result = probe(openers, &conn) => result,
            () = ctx.cancel.cancelled() => return Err(JobError::Cancelled),
        };
        let now = chrono::Utc::now();
        let stored = match &result {
            Ok(success) => {
                let caps: Vec<&str> = success.capabilities.iter().map(|c| c.as_str()).collect();
                let outcome = CheckOutcome::Ok {
                    version: &success.version,
                    capabilities: &caps,
                };
                ctx.db.record_service_check(&conn, &outcome, now).await?
            }
            Err(failure) => {
                let outcome = CheckOutcome::Failed {
                    code: failure.code,
                    version: failure.version.as_deref(),
                };
                ctx.db.record_service_check(&conn, &outcome, now).await?
            }
        };
        if !stored {
            // The result describes a connection that no longer exists.
            tracing::info!(
                service = id,
                "connection changed during the check; checking again"
            );
            continue;
        }
        return Ok(match result {
            Ok(success) => {
                tracing::info!(service = id, version = %success.version, "service check succeeded");
                ReportItem {
                    target,
                    outcome: ItemOutcome::Succeeded,
                    code: None,
                    detail: Some(format!("version {}", success.version)),
                }
            }
            Err(failure) => {
                tracing::warn!(service = id, code = failure.code, detail = %failure.detail, "service check failed");
                ReportItem {
                    target,
                    outcome: ItemOutcome::Failed,
                    code: Some(failure.code.to_owned()),
                    detail: Some(failure.detail),
                }
            }
        });
    }
    Ok(skipped(
        target,
        "the connection kept changing during the check",
    ))
}

async fn probe(openers: Openers<'_>, conn: &ServiceConnection) -> Result<Success, Failure> {
    let (descriptor, client) = instance_client(openers, conn)?;
    let report = adapters::probe(&conn.adapter_type, client).await?;
    Ok(Success {
        version: report.version.to_string(),
        capabilities: descriptor.capabilities,
    })
}

/// The shared client of an instance (one rate limiter per instance, rebuilt
/// when URL or credential change), with its adapter's descriptor. The
/// credential is opened with the key of its class: service credentials
/// with the worker's key, catalog tokens with the web key.
pub(crate) fn instance_client(
    openers: Openers<'_>,
    conn: &ServiceConnection,
) -> Result<(&'static AdapterDescriptor, ServiceClient), Failure> {
    match conn.credential.class {
        SecretClass::ServiceCredential => build_client(openers.clients, conn, FULL_SHARE, || {
            openers.credentials.open(&conn.credential).map_err(|err| {
                Failure::starr(
                    CODE_CREDENTIALS_UNREADABLE,
                    format!("the stored credential cannot be opened: {err}"),
                )
            })
        }),
        SecretClass::CatalogReadToken => match openers.catalog {
            Some(catalog) => catalog.client(conn),
            None => Err(Failure::starr(
                CODE_CATALOG_KEY_UNAVAILABLE,
                "this worker has no web key to open catalog tokens; mount web.key into it (read-only)".to_owned(),
            )),
        },
    }
}

/// A process that holds the whole budget of the services it talks to.
pub const FULL_SHARE: u32 = 100;

/// The client of `conn` from `clients`, built with the credential `open`
/// returns, in a process holding `percent` of the host's budget for it.
pub(crate) fn build_client(
    clients: &ClientPool,
    conn: &ServiceConnection,
    percent: u32,
    open: impl FnOnce() -> Result<SecretString, Failure>,
) -> Result<(&'static AdapterDescriptor, ServiceClient), Failure> {
    let (Some(descriptor), Some(auth)) = (
        adapters::descriptor(&conn.adapter_type),
        adapters::auth(&conn.adapter_type),
    ) else {
        return Err(Failure::starr(
            CODE_ADAPTER_UNKNOWN,
            format!("this Starr build has no adapter {:?}", conn.adapter_type),
        ));
    };
    let credential: SecretString = open()?;
    let base_url = Url::parse(&conn.base_url).map_err(|err| {
        Failure::from(AdapterError::Unknown(format!(
            "stored base URL is invalid: {err}"
        )))
    })?;

    let mut fingerprint = conn.base_url.as_bytes().to_vec();
    fingerprint.push(0);
    fingerprint.extend_from_slice(&conn.credential.ciphertext);
    let client = clients.get_or_build(conn.id, &fingerprint, || {
        ServiceClient::with_options(
            descriptor,
            Endpoint {
                base_url,
                auth,
                credential,
            },
            adapters::client_options(descriptor, percent),
        )
    })?;
    Ok((descriptor, client))
}
