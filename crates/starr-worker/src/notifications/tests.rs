#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use secrecy::SecretString;
use serde_json::{Value, json};
use starr_core::categories::MinimumAvailability;
use starr_core::members::{Category, Entitlement, RequestMode};
use starr_db::Database;
use starr_db::members::NewMember;
use starr_db::notifications::NewChannel;
use starr_db::requests::{
    Actor, DecisionTarget, EventTrigger, NewRequest, RequestFollow, SourceTarget, Unit,
};
use starr_db::services::{NewServiceInstance, StoredCredential};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

const SMTP_PASSWORD: &str = "smtp-pass-0815";
/// base64 of `\0starr\0smtp-pass-0815` (AUTH PLAIN).
const AUTH_PLAIN: &str = "AHN0YXJyAHNtdHAtcGFzcy0wODE1";
const HOOK_TOKEN: &str = "discord-token-4711";

/// A tiny SMTP server: accepts `AUTH PLAIN` with [`AUTH_PLAIN`] only and
/// keeps every message it receives.
#[derive(Clone)]
struct FakeSmtp {
    port: u16,
    messages: Arc<Mutex<Vec<String>>>,
}

impl FakeSmtp {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let messages = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::clone(&messages);
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let store = Arc::clone(&store);
                tokio::spawn(async move {
                    let (read, mut write) = socket.into_split();
                    let mut lines = BufReader::new(read).lines();
                    write.write_all(b"220 fake ESMTP\r\n").await.unwrap();
                    let mut data: Option<String> = None;
                    while let Ok(Some(line)) = lines.next_line().await {
                        if let Some(body) = data.as_mut() {
                            if line == "." {
                                store.lock().unwrap().push(data.take().unwrap());
                                write.write_all(b"250 queued\r\n").await.unwrap();
                            } else {
                                body.push_str(&line);
                                body.push('\n');
                            }
                            continue;
                        }
                        let upper = line.to_ascii_uppercase();
                        let reply: &[u8] = if upper.starts_with("EHLO") {
                            b"250-fake\r\n250 AUTH PLAIN LOGIN\r\n"
                        } else if upper.starts_with("AUTH PLAIN") {
                            if line.ends_with(AUTH_PLAIN) {
                                b"235 ok\r\n"
                            } else {
                                b"535 authentication failed\r\n"
                            }
                        } else if upper.starts_with("DATA") {
                            data = Some(String::new());
                            b"354 go ahead\r\n"
                        } else if upper.starts_with("QUIT") {
                            write.write_all(b"221 bye\r\n").await.unwrap();
                            break;
                        } else {
                            b"250 ok\r\n"
                        };
                        write.write_all(reply).await.unwrap();
                    }
                });
            }
        });
        Self { port, messages }
    }

    fn messages(&self) -> Vec<String> {
        self.messages.lock().unwrap().clone()
    }
}

struct Env {
    db: Database,
    key: CredentialKey,
    mia: i64,
    source: SourceTarget,
    movie: i64,
}

fn seal(key: &CredentialKey, value: &str) -> StoredCredential {
    let sealer = starr_secrets::CredentialSealer::parse(key.recipient()).unwrap();
    let (recipient, ciphertext) = sealer
        .seal(&SecretString::from(value.to_owned()))
        .unwrap()
        .into_parts();
    StoredCredential {
        ciphertext,
        recipient,
        class: starr_core::adapters::SecretClass::ServiceCredential,
    }
}

fn public() -> Url {
    Url::parse("https://starr.example.org").unwrap()
}

impl Env {
    async fn new() -> Self {
        let db = Database::open_in_memory().await.unwrap();
        let key = CredentialKey::ephemeral();
        let mia = db
            .create_member(
                &NewMember {
                    username: "mia",
                    email: "mia@example.com",
                    display_name: "Mia",
                    password_hash: None,
                },
                &[] as &[Entitlement],
                Utc::now(),
            )
            .await
            .unwrap();
        let radarr = db
            .create_service(
                &NewServiceInstance {
                    adapter_type: "radarr",
                    name: "radarr",
                    base_url: "http://radarr",
                    public_url: None,
                    credential: &seal(&key, "k"),
                },
                Utc::now(),
            )
            .await
            .unwrap()
            .id;
        let (movie,): (i64,) = sqlx::query_as(
            "INSERT INTO catalog_item (kind, tmdb_id, title, year, created_at, updated_at)
             VALUES ('movie', 603, '<b>The Matrix</b>', 1999, ?1, ?1) RETURNING id",
        )
        .bind("2026-11-01T00:00:00.000Z")
        .fetch_one(db.pool())
        .await
        .unwrap();
        Self {
            db,
            key,
            mia,
            source: SourceTarget {
                category: Category::Movies,
                service_instance_id: radarr,
                quality_profile_id: 1,
                root_folder_path: "/movies".into(),
                minimum_availability: Some(MinimumAvailability::Released),
                metadata_profile_id: None,
            },
            movie,
        }
    }

    async fn smtp_channel(&self, smtp: &FakeSmtp, password: &str, admin: Option<&str>) -> i64 {
        let settings = serde_json::to_value(EmailSettings {
            host: "127.0.0.1".into(),
            port: smtp.port,
            security: SmtpSecurity::None,
            username: Some("starr".into()),
            from_address: "starr@example.org".into(),
            from_name: Some("Starr".into()),
            admin_address: admin.map(str::to_owned),
        })
        .unwrap();
        self.channel(ChannelKind::Email, "mail", settings, Some(password))
            .await
    }

    async fn channel(
        &self,
        kind: ChannelKind,
        name: &str,
        settings: Value,
        secret: Option<&str>,
    ) -> i64 {
        let secret = secret.map(|s| seal(&self.key, s));
        self.db
            .create_channel(
                &NewChannel {
                    kind,
                    name,
                    enabled: true,
                    settings: &settings,
                    secret: secret.as_ref(),
                    admin_events: &NotificationKind::ADMIN,
                },
                Utc::now(),
            )
            .await
            .unwrap()
            .id
    }

    /// Mia asks (approval mode); the admin approves. Notifies the admins
    /// (pending) and Mia (approved).
    async fn request_and_approve(&self) {
        let id = self
            .db
            .create_request(
                &NewRequest {
                    member_id: self.mia,
                    catalog_item_id: self.movie,
                    follow: RequestFollow::No,
                    units: &[Unit {
                        catalog_item_id: self.movie,
                        season_number: None,
                    }],
                    mode: RequestMode::Approval,
                    source: &self.source,
                },
                Actor {
                    trigger: EventTrigger::Member,
                    name: "mia",
                },
                Utc::now(),
            )
            .await
            .unwrap()
            .request_id;
        self.db
            .approve_requests(
                &[DecisionTarget {
                    request_id: id,
                    member_id: self.mia,
                }],
                std::slice::from_ref(&self.source),
                Actor {
                    trigger: EventTrigger::Admin,
                    name: "root",
                },
                Utc::now(),
            )
            .await
            .unwrap();
    }

    fn ctx(&self, payload: Value) -> JobContext {
        JobContext {
            db: self.db.clone(),
            payload,
            cancel: CancellationToken::new(),
        }
    }

    async fn run(&self) -> Report {
        deliver(&self.ctx(json!({})), &self.key, Some(&public()))
            .await
            .unwrap()
    }

    async fn deliveries(&self) -> Vec<(String, i64, Option<String>, Option<String>)> {
        sqlx::query_as(
            "SELECT state, attempts, error_code, next_attempt_at FROM notification_delivery
             ORDER BY id",
        )
        .fetch_all(self.db.pool())
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn member_email_and_admin_discord_are_delivered() {
    let env = Env::new().await;
    let smtp = FakeSmtp::start().await;
    env.smtp_channel(&smtp, SMTP_PASSWORD, None).await;
    let discord = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/api/webhooks/1/{HOOK_TOKEN}")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&discord)
        .await;
    env.channel(
        ChannelKind::Discord,
        "ops",
        json!({ "host": "discord" }),
        Some(&format!("{}/api/webhooks/1/{HOOK_TOKEN}", discord.uri())),
    )
    .await;
    env.db
        .set_email_preferences(env.mia, &[NotificationKind::RequestApproved])
        .await
        .unwrap();
    env.request_and_approve().await;

    let report = env.run().await;
    assert_eq!(report.failed(), 0, "{report:?}");
    assert_eq!(report.counter("delivered"), Some(2));
    assert!(matches!(report.expectation(), Expectation::Met(_)));

    let messages = smtp.messages();
    assert_eq!(messages.len(), 1);
    let mail = &messages[0];
    assert!(mail.contains("To: mia@example.com"), "{mail}");
    assert!(mail.contains("Request approved"), "{mail}");
    assert!(mail.contains("https://starr.example.org/requests"));
    // Plain text plus HTML (escaping: `render` tests).
    assert!(mail.contains("text/plain"));
    assert!(mail.contains("text/html"));

    let received = discord.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let body: Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(body["allowed_mentions"]["parse"], json!([]));
    let description = body["embeds"][0]["description"].as_str().unwrap();
    assert!(description.starts_with("mia asked for"), "{description}");
    assert_eq!(
        body["embeds"][0]["url"],
        "https://starr.example.org/admin/needs-you"
    );

    let states: Vec<String> = env.deliveries().await.into_iter().map(|d| d.0).collect();
    assert_eq!(states, vec!["delivered", "delivered"]);
    // Nothing is left: the schedule has no work.
    assert!(!env.db.has_due_deliveries(Utc::now()).await.unwrap());
}

#[tokio::test]
async fn rejected_login_fails_at_once_without_leaking_the_password() {
    let env = Env::new().await;
    let smtp = FakeSmtp::start().await;
    let channel = env
        .smtp_channel(&smtp, "wrong-password-1234", Some("ops@example.org"))
        .await;
    env.request_and_approve().await;
    let report = env.run().await;
    assert_eq!(report.failed(), 1);
    assert!(matches!(report.expectation(), Expectation::Unmet(_)));
    let deliveries = env.deliveries().await;
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].0, "failed");
    assert_eq!(deliveries[0].2.as_deref(), Some("credentials_invalid"));
    let record = env.db.get_channel(channel).await.unwrap().unwrap();
    assert_eq!(record.last_ok, Some(false));
    assert!(
        !format!("{report:?}{record:?}").contains("wrong-password-1234"),
        "the password stays secret"
    );
    assert_eq!(smtp.messages(), Vec::<String>::new());
}

#[tokio::test]
async fn webhook_trouble_is_retried_later() {
    let env = Env::new().await;
    let hook = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&hook)
        .await;
    let url = format!("{}/hooks/secret-path-token-99", hook.uri());
    env.channel(ChannelKind::Webhook, "hook", json!({}), Some(&url))
        .await;
    env.request_and_approve().await;
    let report = env.run().await;
    assert_eq!(report.failed(), 1);
    assert_eq!(report.counter("retrying"), Some(1));
    let deliveries = env.deliveries().await;
    assert_eq!(deliveries[0].0, "pending");
    assert_eq!(deliveries[0].1, 1);
    assert_eq!(deliveries[0].2.as_deref(), Some("transient"));
    assert!(deliveries[0].3.is_some(), "due again later");
    assert!(!format!("{report:?}").contains("secret-path-token-99"));
    // Not due right now: a second run sends nothing.
    assert_eq!(env.run().await.counter("delivered"), Some(0));
    // The generic webhook got the structured event.
    let received = hook.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(body["event"], "approval_pending");
    assert_eq!(body["data"]["member"], "mia");
}

#[tokio::test]
async fn member_without_opt_in_gets_no_email_and_test_reaches_the_channel() {
    let env = Env::new().await;
    let smtp = FakeSmtp::start().await;
    let channel = env.smtp_channel(&smtp, SMTP_PASSWORD, None).await;
    env.request_and_approve().await;
    // No opt-in, no admin address: nothing to deliver.
    assert_eq!(env.deliveries().await, vec![]);

    let report = deliver(
        &env.ctx(json!({ "testChannelId": channel })),
        &env.key,
        Some(&public()),
    )
    .await
    .unwrap();
    assert!(
        matches!(report.expectation(), Expectation::Met(_)),
        "{report:?}"
    );
    let messages = smtp.messages();
    assert_eq!(messages.len(), 1);
    // Without an admin address the test goes to the sender.
    assert!(messages[0].contains("To: starr@example.org"));
    assert!(messages[0].contains("Test from Starr"));
    let record = env.db.get_channel(channel).await.unwrap().unwrap();
    assert_eq!(record.last_ok, Some(true));

    let missing = deliver(&env.ctx(json!({ "testChannelId": 999 })), &env.key, None)
        .await
        .unwrap();
    assert!(matches!(missing.expectation(), Expectation::Unmet(_)));
}
impl Env {
    /// The approved request's acquisition fails for good: a second admin
    /// notification.
    async fn fail_acquisition(&self) {
        let (id,): (i64,) = sqlx::query_as("SELECT id FROM acquisition")
            .fetch_one(self.db.pool())
            .await
            .unwrap();
        self.db
            .record_submit_failure(
                id,
                "rejected",
                "no root folder",
                None,
                Actor {
                    trigger: EventTrigger::System,
                    name: "requests.submit",
                },
                Utc::now(),
            )
            .await
            .unwrap();
    }

    async fn hook(&self, server: &MockServer) {
        let url = format!("{}/hooks/secret-path-token-99", server.uri());
        self.channel(ChannelKind::Webhook, "hook", json!({}), Some(&url))
            .await;
    }
}

#[tokio::test]
async fn a_failing_channel_is_paused_for_the_rest_of_the_run() {
    let env = Env::new().await;
    let hook = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&hook)
        .await;
    env.hook(&hook).await;
    env.request_and_approve().await;
    env.fail_acquisition().await;
    let report = env.run().await;
    assert_eq!(report.failed(), 1, "{report:?}");
    assert_eq!(hook.received_requests().await.unwrap().len(), 1);
    let deliveries = env.deliveries().await;
    // The second waits, without having used an attempt.
    assert_eq!(deliveries[0].1 + deliveries[1].1, 1);
    assert!(deliveries.iter().all(|d| d.0 == "pending"));
}

#[tokio::test]
async fn the_last_attempt_fails_for_good() {
    let env = Env::new().await;
    let hook = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&hook)
        .await;
    env.hook(&hook).await;
    env.request_and_approve().await;
    sqlx::query("UPDATE notification_delivery SET attempts = ?")
        .bind(i64::from(rules::MAX_DELIVERY_ATTEMPTS) - 1)
        .execute(env.db.pool())
        .await
        .unwrap();
    let report = env.run().await;
    assert_eq!(report.counter("retrying"), Some(0));
    let deliveries = env.deliveries().await;
    assert_eq!(deliveries[0].0, "failed");
    assert_eq!(deliveries[0].3, None);
}

#[tokio::test]
async fn throttling_postpones_without_using_an_attempt() {
    let env = Env::new().await;
    let hook = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
        .mount(&hook)
        .await;
    env.hook(&hook).await;
    env.request_and_approve().await;
    env.fail_acquisition().await;
    let report = env.run().await;
    assert_eq!(report.failed(), 0, "{report:?}");
    assert_eq!(hook.received_requests().await.unwrap().len(), 1);
    let deliveries = env.deliveries().await;
    assert!(
        deliveries.iter().all(|d| d.0 == "pending" && d.1 == 0),
        "{deliveries:?}"
    );
    assert_eq!(deliveries[0].2.as_deref(), Some("throttled"));
    assert!(deliveries[0].3.is_some());
}

#[tokio::test]
async fn a_member_without_address_fails_the_delivery_not_the_channel() {
    let env = Env::new().await;
    let smtp = FakeSmtp::start().await;
    let channel = env.smtp_channel(&smtp, SMTP_PASSWORD, None).await;
    env.db
        .set_email_preferences(env.mia, &[NotificationKind::RequestApproved])
        .await
        .unwrap();
    env.request_and_approve().await;
    // Suspended after the delivery was planned.
    assert!(env.db.suspend_member(env.mia).await.unwrap());
    let report = env.run().await;
    assert_eq!(report.failed(), 1);
    let deliveries = env.deliveries().await;
    assert_eq!(deliveries[0].0, "failed");
    assert_eq!(deliveries[0].2.as_deref(), Some(CODE_NO_ADDRESS));
    let record = env.db.get_channel(channel).await.unwrap().unwrap();
    assert_eq!(record.last_ok, None);
    assert_eq!(smtp.messages(), Vec::<String>::new());
}

#[test]
fn ip_hosts_greet_with_an_address_literal() {
    assert!(matches!(hello_id("192.168.1.10"), ClientId::Ipv4(_)));
    assert!(matches!(hello_id("[::1]"), ClientId::Ipv6(_)));
    assert!(matches!(hello_id("starr.example.org"), ClientId::Domain(_)));
}
/// Writes paused at the source hold the acquisition: held is not failed,
/// so nobody is told it failed.
#[tokio::test]
async fn held_acquisitions_notify_no_failure() {
    use crate::{JobHandler, SystemJobs};
    use starr_core::jobs::JobKind;
    use starr_db::notifications::Stream;

    let env = Env::new().await;
    env.db
        .set_writes_paused(env.source.service_instance_id, Some("root"), Utc::now())
        .await
        .unwrap();
    env.request_and_approve().await;
    let jobs = SystemJobs::new(CredentialKey::ephemeral());
    for _ in 0..2 {
        jobs.run(JobKind::RequestsSubmit, env.ctx(json!({})))
            .await
            .unwrap();
    }
    let admin: Vec<NotificationKind> = env
        .db
        .list_notifications(Stream::Admin, None, 50)
        .await
        .unwrap()
        .into_iter()
        .map(|n| n.kind)
        .collect();
    assert_eq!(admin, vec![NotificationKind::ApprovalPending]);
    let mine: Vec<NotificationKind> = env
        .db
        .list_notifications(Stream::Member(env.mia), None, 50)
        .await
        .unwrap()
        .into_iter()
        .map(|n| n.kind)
        .collect();
    assert_eq!(mine, vec![NotificationKind::RequestApproved]);
}
