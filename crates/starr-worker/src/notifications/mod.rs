//! `notifications.deliver` (spec 0003 O1, O2): sends due deliveries
//! through their channel adapters, each delivery with its own retries.
//!
//! Channels are adapters in the ADR 0009 sense without being service
//! instances: they have no version and no probe, but they get their HTTP
//! client from `starr-adapter-kit` (webhook URLs, which carry tokens, are
//! redacted there) and map every failure to the [`AdapterError`] taxonomy,
//! on which retry and display decide (`notifications::retry_delay`).
//! Channel secrets are service credentials (ADR 0003 amendment 1): only
//! this worker module opens them.

pub mod render;

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::extension::ClientId;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use secrecy::{ExposeSecret, SecretString};
use starr_adapter_kit::{Redactor, ServiceClient};
use starr_core::adapters::{AdapterError, RateLimit};
use starr_core::jobs::{Expectation, ItemOutcome, Report, ReportItem};
use starr_core::notifications::{self as rules, ChannelKind, NotificationKind, Payload};
pub use starr_core::notifications::{EmailSettings, SmtpSecurity};
use starr_db::notifications::{ChannelConnection, DueDelivery};
use url::Url;

use crate::secrets::CredentialKey;
use crate::services::CODE_CREDENTIALS_UNREADABLE;
use crate::{JobContext, JobError};

/// Most deliveries sent per run; the rest wait for the next one.
const BATCH: u32 = 100;
/// Time one run may spend sending, so a slow channel cannot keep the
/// (single) worker from requests and sync; the rest waits for the next run.
const RUN_BUDGET: Duration = Duration::from_secs(60);
/// Error code when a member has no (active) email address.
pub const CODE_NO_ADDRESS: &str = "no_address";
/// Error code when the channel's stored settings are unusable.
pub const CODE_SETTINGS_INVALID: &str = "settings_invalid";
const SMTP_TIMEOUT: Duration = Duration::from_secs(20);
/// Webhooks are posted one at a time; Discord allows about five per two
/// seconds per webhook.
const WEBHOOK_RATE: RateLimit = RateLimit {
    requests: 5,
    per: Duration::from_secs(2),
};

/// Why one send failed: taxonomy code, readable detail (no secrets), and
/// the adapter error retries decide on (none for Starr-side problems such
/// as a missing address, which no retry fixes).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Failure {
    code: &'static str,
    detail: String,
    cause: Option<Box<AdapterError>>,
}

impl Failure {
    fn starr(code: &'static str, detail: String) -> Self {
        Self {
            code,
            detail,
            cause: None,
        }
    }

    /// Trouble that may pass (network, throttling): the channel is paused
    /// for the rest of the run instead of being tried again and again.
    fn passing(&self) -> bool {
        matches!(
            self.cause.as_deref(),
            Some(AdapterError::Transient(_) | AdapterError::Throttled { .. })
        )
    }

    /// When to try again after attempt `attempt` (1-based).
    fn retry(&self, attempt: u32) -> Option<Duration> {
        self.cause
            .as_ref()
            .and_then(|err| rules::retry_delay(attempt, err))
    }
}

impl From<AdapterError> for Failure {
    fn from(err: AdapterError) -> Self {
        Self {
            code: err.code(),
            detail: err.to_string(),
            cause: Some(Box::new(err)),
        }
    }
}

/// Where a message goes on its channel.
#[derive(Debug, Clone, Copy)]
enum Recipient<'a> {
    /// An email address.
    Address(&'a str),
    /// The channel's own target (webhook).
    Channel,
}

/// What is sent: words plus, for webhooks, the structured event.
struct Outgoing<'a> {
    kind: Option<NotificationKind>,
    payload: Option<&'a Payload>,
    rendered: render::Rendered,
    created_at: String,
}

/// Map an SMTP failure to the taxonomy, without the password.
fn smtp_error(err: &lettre::transport::smtp::Error, redactor: &Redactor) -> AdapterError {
    let detail = redactor.redact(&err.to_string());
    match err.status().map(|code| code.to_string()) {
        // 535: authentication failed; 530: authentication required.
        Some(code) if code == "535" || code == "530" => AdapterError::CredentialsInvalid,
        _ if err.is_permanent() || err.is_client() || err.is_tls() => {
            AdapterError::Rejected(format!("SMTP: {detail}"))
        }
        _ => AdapterError::Transient(format!("SMTP: {detail}")),
    }
}

fn mailbox(address: &str, name: Option<&str>) -> Result<Mailbox, AdapterError> {
    let address = address
        .parse()
        .map_err(|_| AdapterError::Rejected(format!("{address:?} is not an email address")))?;
    Ok(Mailbox::new(name.map(str::to_owned), address))
}

async fn send_email(
    settings: &EmailSettings,
    password: Option<SecretString>,
    to: &str,
    email: &render::Email,
    hello: &str,
) -> Result<(), AdapterError> {
    let message = Message::builder()
        .from(mailbox(
            &settings.from_address,
            settings.from_name.as_deref(),
        )?)
        .to(mailbox(to, None)?)
        .subject(&email.subject)
        .multipart(MultiPart::alternative_plain_html(
            email.text.clone(),
            email.html.clone(),
        ))
        .map_err(|err| AdapterError::Rejected(format!("cannot build the email: {err}")))?;
    let builder = match settings.security {
        SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&settings.host),
        SmtpSecurity::Starttls => {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&settings.host)
        }
        SmtpSecurity::None => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
            &settings.host,
        )),
    }
    .map_err(|err| AdapterError::Rejected(format!("SMTP TLS setup: {err}")))?;
    let mut builder = builder
        .port(settings.port)
        .timeout(Some(SMTP_TIMEOUT))
        .hello_name(hello_id(hello));
    let mut secrets = Vec::new();
    if let (Some(username), Some(password)) = (settings.username.as_deref(), password) {
        builder = builder.credentials(Credentials::new(
            username.to_owned(),
            password.expose_secret().to_owned(),
        ));
        secrets.push(password);
    }
    let redactor = Redactor::new(secrets);
    let transport = builder.build();
    transport
        .send(message)
        .await
        .map(|_| ())
        .map_err(|err| smtp_error(&err, &redactor))
}

/// The webhook client of each channel within one run, so the rate limit
/// and a 429 pause apply across its deliveries.
type Clients = HashMap<i64, ServiceClient>;

/// Send one message on `channel`.
async fn send(
    channel: &ChannelConnection,
    credentials: &CredentialKey,
    clients: &mut Clients,
    recipient: Recipient<'_>,
    outgoing: &Outgoing<'_>,
    hello: &str,
) -> Result<(), Failure> {
    let secret = match &channel.secret {
        Some(stored) => Some(credentials.open(stored).map_err(|_| {
            Failure::starr(
                CODE_CREDENTIALS_UNREADABLE,
                "the channel's secret cannot be opened with this secret.key; enter it again"
                    .to_owned(),
            )
        })?),
        None => None,
    };
    let invalid = |what: &str| {
        Failure::starr(
            CODE_SETTINGS_INVALID,
            format!("the channel settings are incomplete: {what}"),
        )
    };
    match channel.kind {
        ChannelKind::Email => {
            let settings: EmailSettings = serde_json::from_value(channel.settings.clone())
                .map_err(|_| invalid("SMTP server"))?;
            let Recipient::Address(to) = recipient else {
                return Err(invalid("no recipient address"));
            };
            let email = render::email(&outgoing.rendered);
            Ok(send_email(&settings, secret, to, &email, hello).await?)
        }
        ChannelKind::Webhook | ChannelKind::Discord => {
            let client = if let Some(client) = clients.get(&channel.id) {
                client.clone()
            } else {
                let url = secret.ok_or_else(|| invalid("webhook URL"))?;
                let client = ServiceClient::secret_url(channel.kind.as_str(), WEBHOOK_RATE, &url)?;
                clients.insert(channel.id, client.clone());
                client
            };
            let body = if channel.kind == ChannelKind::Discord {
                render::discord(&outgoing.rendered)
            } else {
                render::webhook(
                    outgoing.kind,
                    outgoing.payload,
                    &outgoing.rendered,
                    &outgoing.created_at,
                )
            };
            Ok(client.post_to_url(&body).await?)
        }
    }
}

fn admin_address(channel: &ChannelConnection) -> Option<String> {
    channel
        .settings
        .get("adminAddress")
        .and_then(serde_json::Value::as_str)
        .filter(|a| !a.is_empty())
        .map(str::to_owned)
}

/// The EHLO name: the public host, or a neutral one.
fn hello_name(public_url: Option<&Url>) -> String {
    public_url
        .and_then(Url::host_str)
        .unwrap_or("starr.localdomain")
        .to_owned()
}

/// EHLO with an address literal for an IP (strict servers refuse a bare
/// IP as a domain).
fn hello_id(hello: &str) -> ClientId {
    match hello.trim_matches(['[', ']']).parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ClientId::Ipv4(ip),
        Ok(IpAddr::V6(ip)) => ClientId::Ipv6(ip),
        Err(_) => ClientId::Domain(hello.to_owned()),
    }
}

fn ts(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Run `notifications.deliver`: a test on one channel (payload
/// `{"testChannelId": n}`), or every due delivery.
pub(crate) async fn deliver(
    ctx: &JobContext,
    credentials: &CredentialKey,
    public_url: Option<&Url>,
) -> Result<Report, JobError> {
    if let Some(id) = ctx
        .payload
        .get("testChannelId")
        .and_then(serde_json::Value::as_i64)
    {
        return test(ctx, credentials, public_url, id).await;
    }
    let hello = hello_name(public_url);
    let due = ctx.db.due_deliveries(Utc::now(), BATCH).await?;
    let total = due.len() as u64;
    let started = Instant::now();
    let mut clients = Clients::new();
    // Channels with passing trouble in this run: their other deliveries
    // wait for the next run without using up an attempt.
    let mut paused: HashSet<i64> = HashSet::new();
    let (mut ok, mut failed, mut skipped, mut retrying) = (0u64, 0u64, 0u64, 0u64);
    let mut items = Vec::new();
    for delivery in &due {
        if ctx.cancel.is_cancelled() {
            return Err(JobError::Cancelled);
        }
        let target = format!(
            "{} #{} → {}",
            delivery.notification.kind.as_str(),
            delivery.notification.id,
            delivery.channel.name
        );
        let skip = if paused.contains(&delivery.channel.id) {
            Some("the channel had trouble earlier in this run; next run")
        } else if started.elapsed() > RUN_BUDGET {
            Some("the run used up its time; next run")
        } else {
            None
        };
        if let Some(reason) = skip {
            skipped += 1;
            items.push(item(
                target,
                ItemOutcome::Skipped,
                None,
                Some(reason.to_owned()),
            ));
            continue;
        }
        let outcome =
            deliver_one(ctx, credentials, &mut clients, public_url, &hello, delivery).await?;
        match outcome {
            Sent::Ok => {
                ok += 1;
                items.push(item(target, ItemOutcome::Succeeded, None, None));
            }
            Sent::Postponed(failure) => {
                paused.insert(delivery.channel.id);
                skipped += 1;
                let detail = format!("{} (postponed)", failure.detail);
                items.push(item(
                    target,
                    ItemOutcome::Skipped,
                    Some(failure.code),
                    Some(detail),
                ));
            }
            Sent::Failed { failure, retry } => {
                if failure.passing() {
                    paused.insert(delivery.channel.id);
                }
                failed += 1;
                retrying += u64::from(retry);
                let detail = if retry {
                    format!("{} (will retry)", failure.detail)
                } else {
                    failure.detail
                };
                items.push(item(
                    target,
                    ItemOutcome::Failed,
                    Some(failure.code),
                    Some(detail),
                ));
            }
        }
    }
    let expectation = if total == 0 {
        Expectation::NotApplicable
    } else if failed == 0 && skipped == 0 {
        Expectation::Met(format!("{ok} of {total} deliveries sent"))
    } else {
        Expectation::Unmet(format!(
            "{ok} of {total} deliveries sent, {failed} failed ({retrying} will retry), {skipped} left for the next run"
        ))
    };
    let mut report = Report::new(total, ok, failed, skipped, expectation)?
        .with_counter("delivered", ok)
        .with_counter("retrying", retrying);
    for item in items {
        report = report.with_item(item);
    }
    Ok(report)
}

fn item(
    target: String,
    outcome: ItemOutcome,
    code: Option<&str>,
    detail: Option<String>,
) -> ReportItem {
    ReportItem {
        target,
        outcome,
        code: code.map(str::to_owned),
        detail,
    }
}

/// How one delivery went.
enum Sent {
    Ok,
    /// Throttled: due again later, no attempt used.
    Postponed(Failure),
    Failed {
        failure: Failure,
        /// Due again later (attempt used).
        retry: bool,
    },
}

/// Send one delivery and record the outcome.
async fn deliver_one(
    ctx: &JobContext,
    credentials: &CredentialKey,
    clients: &mut Clients,
    public_url: Option<&Url>,
    hello: &str,
    delivery: &DueDelivery,
) -> Result<Sent, JobError> {
    let n = &delivery.notification;
    let address = if n.kind.is_admin() {
        admin_address(&delivery.channel)
    } else {
        delivery.member_email.clone()
    };
    let recipient = match (delivery.channel.kind, address.as_deref()) {
        (ChannelKind::Email, Some(address)) => Ok(Recipient::Address(address)),
        (ChannelKind::Email, None) => Err(Failure::starr(
            CODE_NO_ADDRESS,
            "no email address to send to".to_owned(),
        )),
        _ => Ok(Recipient::Channel),
    };
    let outgoing = Outgoing {
        kind: Some(n.kind),
        payload: Some(&n.payload),
        rendered: render::render(n.kind, &n.payload, public_url),
        created_at: ts(n.created_at),
    };
    let result = match recipient {
        Ok(recipient) => {
            send(
                &delivery.channel,
                credentials,
                clients,
                recipient,
                &outgoing,
                hello,
            )
            .await
        }
        Err(failure) => Err(failure),
    };
    let now = Utc::now();
    let failure = match result {
        Ok(()) => {
            ctx.db.record_delivered(delivery.id, now).await?;
            ctx.db
                .record_channel_result(delivery.channel.id, None, now)
                .await?;
            return Ok(Sent::Ok);
        }
        Err(failure) => failure,
    };
    if let Some(AdapterError::Throttled { retry_after }) = failure.cause.as_deref() {
        let wait = retry_after.unwrap_or(rules::DELIVERY_BACKOFF);
        let at = now + chrono::TimeDelta::from_std(wait).unwrap_or(chrono::TimeDelta::MAX);
        tracing::info!(delivery = delivery.id, channel = %delivery.channel.name, ?wait, "notification delivery throttled");
        ctx.db
            .postpone_delivery(delivery.id, failure.code, &failure.detail, at, now)
            .await?;
        return Ok(Sent::Postponed(failure));
    }
    let attempt = delivery.attempts.saturating_add(1);
    let retry = failure.retry(attempt).and_then(|delay| {
        chrono::TimeDelta::from_std(delay)
            .ok()
            .map(|delay| now + delay)
    });
    tracing::warn!(
        delivery = delivery.id,
        channel = %delivery.channel.name,
        code = failure.code,
        detail = %failure.detail,
        retry = ?retry,
        "notification delivery failed"
    );
    ctx.db
        .record_delivery_failure(delivery.id, failure.code, &failure.detail, retry, now)
        .await?;
    // A member without an address says nothing about the channel.
    if failure.code != CODE_NO_ADDRESS {
        ctx.db
            .record_channel_result(
                delivery.channel.id,
                Some((failure.code, &failure.detail)),
                now,
            )
            .await?;
    }
    Ok(Sent::Failed {
        failure,
        retry: retry.is_some(),
    })
}

/// Send the test message on channel `id` and record the result on it.
async fn test(
    ctx: &JobContext,
    credentials: &CredentialKey,
    public_url: Option<&Url>,
    id: i64,
) -> Result<Report, JobError> {
    let Some(channel) = ctx.db.channel_connection(id).await? else {
        return Ok(Report::new(
            0,
            0,
            0,
            0,
            Expectation::Unmet(format!("channel {id} does not exist (deleted meanwhile?)")),
        )?);
    };
    let outgoing = Outgoing {
        kind: None,
        payload: None,
        rendered: render::test_message(public_url),
        created_at: ts(Utc::now()),
    };
    // Email: to the admin address, else to the sender itself.
    let address = admin_address(&channel).or_else(|| {
        channel
            .settings
            .get("fromAddress")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    });
    let recipient = match (channel.kind, address.as_deref()) {
        (ChannelKind::Email, Some(address)) => Recipient::Address(address),
        _ => Recipient::Channel,
    };
    let result = send(
        &channel,
        credentials,
        &mut Clients::new(),
        recipient,
        &outgoing,
        &hello_name(public_url),
    )
    .await;
    let now = Utc::now();
    let target = format!("test → {}", channel.name);
    match result {
        Ok(()) => {
            ctx.db.record_channel_result(id, None, now).await?;
            Ok(
                Report::new(1, 1, 0, 0, Expectation::Met("test message sent".to_owned()))?
                    .with_item(ReportItem {
                        target,
                        outcome: ItemOutcome::Succeeded,
                        code: None,
                        detail: None,
                    }),
            )
        }
        Err(failure) => {
            ctx.db
                .record_channel_result(id, Some((failure.code, &failure.detail)), now)
                .await?;
            Ok(Report::new(
                1,
                0,
                1,
                0,
                Expectation::Unmet(format!("test message failed: {}", failure.detail)),
            )?
            .with_item(ReportItem {
                target,
                outcome: ItemOutcome::Failed,
                code: Some(failure.code.to_owned()),
                detail: Some(failure.detail),
            }))
        }
    }
}

#[cfg(test)]
mod tests;
