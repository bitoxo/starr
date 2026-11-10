//! Notifications (spec 0003 O1, O2, D16; domain model §7). Pure rules:
//! which request transitions notify whom, the kinds and channels, the
//! structured payload, and when a failed delivery is retried. Storage lives
//! in `starr-db`, rendering and sending in the worker's channel adapters
//! (lesson 30: content is structured and rendered only in the channel).
//!
//! Channels are not service adapters: they have no version, no probe and
//! no catalog. They reuse the adapter error taxonomy ([`AdapterError`]),
//! so retry and display decide on the variant like everywhere else.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::adapters::AdapterError;
use crate::members::Category;
use crate::requests::RequestState;

macro_rules! names {
    ($ty:ident { $($variant:ident => $name:literal),+ $(,)? }) => {
        impl $ty {
            pub const ALL: [Self; [$(stringify!($variant)),+].len()] = [$(Self::$variant),+];

            /// Stable name used in the database and the API.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }

            #[must_use]
            pub fn parse(raw: &str) -> Option<Self> {
                Self::ALL.into_iter().find(|v| v.as_str() == raw)
            }
        }
    };
}

/// What a notification is about. Member kinds go to the member's own
/// stream, admin kinds to the shared admin stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NotificationKind {
    /// An admin approved the member's request.
    RequestApproved,
    /// An admin rejected it; the payload carries the reason.
    RequestRejected,
    /// An admin revoked it (spec 0004 R3); the payload carries the reason.
    RequestRevoked,
    /// Something of it is watchable (a season, partly).
    RequestPartiallyAvailable,
    /// All of it is watchable.
    RequestAvailable,
    /// Its acquisition failed; an admin has to look.
    RequestFailed,
    /// `follows.check` requested a new season of a followed series in the
    /// member's name.
    FollowRequested,
    /// `follows.check` found more new scenes for a performer or studio
    /// follow than the weekly cap allows (spec 0007 Q4): the rest wait for
    /// the member. Once per follow and week; the payload names the
    /// followed performer or studio as `title`.
    FollowCapped,
    /// Starr needs the member's password once to set up or update an
    /// account in a service ("activate with your password", ADR 0010);
    /// the payload names the service as `title`.
    AccountActivate,
    /// Admin: a request waits for approval (D16).
    ApprovalPending,
    /// Admin: an acquisition failed for good.
    AcquisitionFailed,
    /// Admin: the source downloaded a title but its import is blocked
    /// (once per acquisition); the payload carries the source's message.
    ImportBlocked,
    /// Admin: an account Starr manages gained administrator rights in the
    /// service (D2, D13); Starr keeps its hands off. The payload names the
    /// service as `title` and the member.
    AccountPrivileged,
    /// Admin: the service locked an account Starr manages after failed
    /// logins (D10); Starr never lifts it. Payload as above.
    AccountLocked,
}

names!(NotificationKind {
    RequestApproved => "request_approved",
    RequestRejected => "request_rejected",
    RequestRevoked => "request_revoked",
    RequestPartiallyAvailable => "request_partially_available",
    RequestAvailable => "request_available",
    RequestFailed => "request_failed",
    FollowRequested => "follow_requested",
    FollowCapped => "follow_capped",
    AccountActivate => "account_activate",
    ApprovalPending => "approval_pending",
    AcquisitionFailed => "acquisition_failed",
    ImportBlocked => "import_blocked",
    AccountPrivileged => "account_privileged",
    AccountLocked => "account_locked",
});

impl NotificationKind {
    /// Kinds of a member's stream, in display order.
    pub const MEMBER: [Self; 9] = [
        Self::AccountActivate,
        Self::FollowRequested,
        Self::FollowCapped,
        Self::RequestApproved,
        Self::RequestRejected,
        Self::RequestRevoked,
        Self::RequestPartiallyAvailable,
        Self::RequestAvailable,
        Self::RequestFailed,
    ];

    /// Kinds of the admin stream, in display order.
    pub const ADMIN: [Self; 5] = [
        Self::ApprovalPending,
        Self::AcquisitionFailed,
        Self::ImportBlocked,
        Self::AccountPrivileged,
        Self::AccountLocked,
    ];

    /// Whether the kind belongs to the admin stream.
    #[must_use]
    pub const fn is_admin(self) -> bool {
        matches!(
            self,
            Self::ApprovalPending
                | Self::AcquisitionFailed
                | Self::ImportBlocked
                | Self::AccountPrivileged
                | Self::AccountLocked
        )
    }
}

/// Where admin-configured notifications are sent besides the in-app list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelKind {
    /// An SMTP server: member emails, and the admin stream to one address.
    Email,
    /// A generic webhook receiving JSON.
    Webhook,
    /// A Discord webhook (Discord's message format).
    Discord,
}

names!(ChannelKind {
    Email => "email",
    Webhook => "webhook",
    Discord => "discord",
});

/// The notifications a change of a request's state causes: `before` is
/// `None` when the request was just created. A request created waiting for
/// approval notifies the admins (D16); a member hears about decisions and
/// progress, never about what they did themselves (creating, withdrawing,
/// cancelling, an automatic approval at creation), and not when a title
/// left the library (spec 0004: it simply shows as requestable again).
#[must_use]
pub fn for_transition(
    before: Option<RequestState>,
    after: RequestState,
) -> Option<NotificationKind> {
    use RequestState as S;
    if before == Some(after) {
        return None;
    }
    match (before, after) {
        (None, S::PendingApproval) => Some(NotificationKind::ApprovalPending),
        (None, _) => None,
        (Some(S::PendingApproval), S::Approved | S::Submitted | S::InProgress) => {
            Some(NotificationKind::RequestApproved)
        }
        (_, S::Rejected) => Some(NotificationKind::RequestRejected),
        (_, S::Revoked) => Some(NotificationKind::RequestRevoked),
        (_, S::PartiallyAvailable) => Some(NotificationKind::RequestPartiallyAvailable),
        (_, S::Available) => Some(NotificationKind::RequestAvailable),
        (_, S::Failed) => Some(NotificationKind::RequestFailed),
        _ => None,
    }
}

/// The structured content of a notification. Never names another member:
/// `member` is set only for the admin stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload {
    /// Movie or series title.
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i64>,
    /// Category name (`movies`, `series`, …). Adult titles are not named
    /// outside Starr (lesson 30).
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquisition_id: Option<i64>,
    /// Seasons the notification is about (series).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seasons: Vec<u32>,
    /// Reject or revoke reason, why an acquisition failed, or the source's
    /// message about a blocked import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Admin stream: who asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
}

impl Payload {
    /// Whether the notification is about a private category (adult, spec
    /// 0007 privacy model). Account notifications carry `account` as their
    /// category and are not private.
    #[must_use]
    pub fn is_private(&self) -> bool {
        Category::parse(&self.category).is_some_and(crate::categories::is_private)
    }

    /// Whether channels outside Starr may show what the notification names:
    /// the title (movie, series, artist; for follows the followed name), its
    /// year, seasons, the reason, the member and a deep link (Q8 A). Private
    /// categories never: external renders stay neutral.
    #[must_use]
    pub fn title_shareable(&self) -> bool {
        !self.is_private()
    }

    /// The payload with everything that names the title stripped: what a
    /// viewer behind a closed gate or an external channel may see of a
    /// private notification (the kind, the category, the ids).
    #[must_use]
    pub fn neutral(&self) -> Self {
        Self {
            title: String::new(),
            year: None,
            category: self.category.clone(),
            request_id: self.request_id,
            acquisition_id: self.acquisition_id,
            seasons: Vec::new(),
            reason: None,
            member: None,
        }
    }
}

/// How the SMTP connection is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SmtpSecurity {
    /// Plain connection upgraded with STARTTLS (port 587).
    Starttls,
    /// TLS from the first byte (port 465).
    Tls,
    /// No encryption: only for a relay on the same host or network.
    None,
}

/// The non-secret settings of the email channel, as stored: the
/// contract between the web side (validates, stores) and the worker
/// (sends). The password is the channel's sealed secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailSettings {
    pub host: String,
    pub port: u16,
    pub security: SmtpSecurity,
    /// Login; the password is the channel's sealed secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub from_address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_name: Option<String>,
    /// Where admin kinds go by email; none: admins get no email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_address: Option<String>,
}

/// Total attempts per delivery before it counts as failed.
pub const MAX_DELIVERY_ATTEMPTS: u32 = 5;

/// Delay before the first retry; doubles with every further attempt.
pub const DELIVERY_BACKOFF: Duration = Duration::from_mins(1);

/// When to try a delivery again after attempt `attempt` (1-based) failed
/// with `error`: only trouble that may pass (network, throttling) is
/// retried, up to [`MAX_DELIVERY_ATTEMPTS`]. Rejected credentials, a
/// refused message or a vanished webhook fail at once.
#[must_use]
pub fn retry_delay(attempt: u32, error: &AdapterError) -> Option<Duration> {
    if attempt >= MAX_DELIVERY_ATTEMPTS {
        return None;
    }
    let backoff = DELIVERY_BACKOFF.saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)));
    match error {
        AdapterError::Transient(_) => Some(backoff),
        AdapterError::Throttled { retry_after } => {
            Some(retry_after.map_or(backoff, |after| after.max(Duration::from_secs(1))))
        }
        AdapterError::CredentialsInvalid
        | AdapterError::NotFound(_)
        | AdapterError::Rejected(_)
        | AdapterError::VersionUnsupported { .. }
        | AdapterError::Unknown(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use NotificationKind as K;
    use RequestState as S;

    #[test]
    fn names_round_trip() {
        for kind in K::ALL {
            assert_eq!(K::parse(kind.as_str()), Some(kind));
            assert_eq!(kind.is_admin(), K::ADMIN.contains(&kind));
            assert_eq!(!kind.is_admin(), K::MEMBER.contains(&kind));
        }
        for kind in ChannelKind::ALL {
            assert_eq!(ChannelKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(K::parse("nope"), None);
    }

    #[test]
    fn creation_notifies_admins_only_when_waiting() {
        assert_eq!(
            for_transition(None, S::PendingApproval),
            Some(K::ApprovalPending)
        );
        assert_eq!(for_transition(None, S::Approved), None);
        assert_eq!(for_transition(None, S::InProgress), None);
    }

    #[test]
    fn decisions_and_progress_notify_the_member() {
        let cases = [
            (S::PendingApproval, S::Approved, Some(K::RequestApproved)),
            (S::PendingApproval, S::Submitted, Some(K::RequestApproved)),
            (S::PendingApproval, S::InProgress, Some(K::RequestApproved)),
            (S::PendingApproval, S::Rejected, Some(K::RequestRejected)),
            (S::PendingApproval, S::Cancelled, None),
            (S::Approved, S::Submitted, None),
            (S::Submitted, S::InProgress, None),
            (
                S::InProgress,
                S::PartiallyAvailable,
                Some(K::RequestPartiallyAvailable),
            ),
            (
                S::PartiallyAvailable,
                S::Available,
                Some(K::RequestAvailable),
            ),
            (S::InProgress, S::Available, Some(K::RequestAvailable)),
            (S::Submitted, S::Failed, Some(K::RequestFailed)),
            (S::Failed, S::Approved, None),
            (S::InProgress, S::Revoked, Some(K::RequestRevoked)),
            (S::Available, S::Revoked, Some(K::RequestRevoked)),
            (S::PendingApproval, S::Revoked, Some(K::RequestRevoked)),
            // Own cancel and a title leaving the library stay silent.
            (S::Submitted, S::Cancelled, None),
            (S::Available, S::Removed, None),
            (S::Available, S::Available, None),
        ];
        for (before, after, expected) in cases {
            assert_eq!(
                for_transition(Some(before), after),
                expected,
                "{before:?} -> {after:?}"
            );
        }
    }

    #[test]
    fn only_passing_trouble_is_retried_with_backoff() {
        let transient = AdapterError::Transient("timeout".into());
        assert_eq!(retry_delay(1, &transient), Some(Duration::from_mins(1)));
        assert_eq!(retry_delay(3, &transient), Some(Duration::from_mins(4)));
        assert_eq!(retry_delay(MAX_DELIVERY_ATTEMPTS, &transient), None);
        let throttled = AdapterError::Throttled {
            retry_after: Some(Duration::from_secs(20)),
        };
        assert_eq!(retry_delay(1, &throttled), Some(Duration::from_secs(20)));
        assert_eq!(retry_delay(1, &AdapterError::CredentialsInvalid), None);
        assert_eq!(retry_delay(1, &AdapterError::Rejected("bad".into())), None);
    }

    #[test]
    fn adult_titles_stay_inside_starr() {
        let payload = Payload {
            category: "adult".into(),
            ..Payload::default()
        };
        assert!(!payload.title_shareable());
        assert!(
            Payload {
                category: "movies".into(),
                ..Payload::default()
            }
            .title_shareable()
        );
        // Account notifications name the service as before.
        assert!(
            Payload {
                category: "account".into(),
                ..Payload::default()
            }
            .title_shareable()
        );
    }

    #[test]
    fn neutral_payload_keeps_only_kind_category_and_ids() {
        let payload = Payload {
            title: "Scene A".into(),
            year: Some(2026),
            category: "adult".into(),
            request_id: Some(7),
            acquisition_id: Some(9),
            seasons: vec![1],
            reason: Some("Performer P".into()),
            member: Some("ada".into()),
        };
        assert_eq!(
            payload.neutral(),
            Payload {
                category: "adult".into(),
                request_id: Some(7),
                acquisition_id: Some(9),
                ..Payload::default()
            }
        );
    }
}
