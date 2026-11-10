//! Members, entitlements, invitations and password tokens (domain model §1–2,
//! ADR 0006). Pure rules; storage lives in `starr-db`.

use chrono::TimeDelta;
use serde::{Deserialize, Serialize};

/// Lifetime of a set-password or reset link.
pub const PASSWORD_TOKEN_LIFETIME: TimeDelta = TimeDelta::days(7);
/// Default lifetime of an invitation link.
pub const INVITATION_DEFAULT_DAYS: u32 = 7;
/// Longest lifetime an admin may give an invitation link.
pub const INVITATION_MAX_DAYS: u32 = 30;
/// Maximum email length in characters (RFC 5321 path limit).
pub const EMAIL_MAX_CHARS: usize = 254;

/// Fixed categories (domain model §2). A new category is a code change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Movies,
    Series,
    Music,
    Adult,
}

impl Category {
    /// All categories in display order.
    pub const ALL: [Self; 4] = [Self::Movies, Self::Series, Self::Music, Self::Adult];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Movies => "movies",
            Self::Series => "series",
            Self::Music => "music",
            Self::Adult => "adult",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == value)
    }
}

/// Whether a member's requests in a category need an admin's approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestMode {
    Auto,
    /// Default, also for existing members when a category is introduced.
    #[default]
    Approval,
}

impl RequestMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Approval => "approval",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "approval" => Some(Self::Approval),
            _ => None,
        }
    }
}

/// A member's entitlement for one whole category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entitlement {
    pub category: Category,
    pub enabled: bool,
    pub request_mode: RequestMode,
}

impl Entitlement {
    /// What a member has in `category` without a stored entitlement: nothing,
    /// requests needing approval. Adult in particular is off by default.
    #[must_use]
    pub const fn default_for(category: Category) -> Self {
        Self {
            category,
            enabled: false,
            request_mode: RequestMode::Approval,
        }
    }
}

/// One entitlement per category, in [`Category::ALL`] order: the given ones,
/// defaults for the rest. Later duplicates of a category win.
#[must_use]
pub fn complete_entitlements(given: &[Entitlement]) -> Vec<Entitlement> {
    Category::ALL
        .into_iter()
        .map(|category| {
            given
                .iter()
                .rev()
                .find(|e| e.category == category)
                .copied()
                .unwrap_or(Entitlement::default_for(category))
        })
        .collect()
}

/// Member status (domain model §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemberStatus {
    Active,
    /// Temporarily blocked; sessions ended.
    Suspended,
    /// Deletion pending (grace period) or completed (tombstone).
    Retired,
}

impl MemberStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Suspended => "suspended",
            Self::Retired => "retired",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "suspended" => Some(Self::Suspended),
            "retired" => Some(Self::Retired),
            _ => None,
        }
    }
}

/// Purpose of a password token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PasswordTokenPurpose {
    /// First password of a member the admin created.
    Set,
    /// Admin-triggered reset.
    Reset,
}

impl PasswordTokenPurpose {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Reset => "reset",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "set" => Some(Self::Set),
            "reset" => Some(Self::Reset),
            _ => None,
        }
    }
}

/// Why an email address was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("email must look like name@domain.tld and have at most {EMAIL_MAX_CHARS} characters")]
pub struct EmailError;

/// Normalize (trim, lowercase) and sanity-check an email address. Deliberately
/// loose: one `@`, a non-empty local part, a dotted domain, no whitespace.
/// Real validation needs a delivered mail, which arrives with notifications.
pub fn parse_email(input: &str) -> Result<String, EmailError> {
    let email = input.trim().to_lowercase();
    let Some((local, domain)) = email.split_once('@') else {
        return Err(EmailError);
    };
    let domain_ok = domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@');
    let ok = !local.is_empty()
        && domain_ok
        && email.chars().count() <= EMAIL_MAX_CHARS
        && !email.chars().any(char::is_whitespace);
    if ok { Ok(email) } else { Err(EmailError) }
}

/// Lifecycle of an invitation as shown to admins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InvitationState {
    Pending,
    Accepted,
    Revoked,
    Expired,
}

/// Derive the state of an invitation from its timestamps. Accepted and
/// revoked win over expiry: they are final.
#[must_use]
pub fn invitation_state(used: bool, revoked: bool, expired: bool) -> InvitationState {
    if used {
        InvitationState::Accepted
    } else if revoked {
        InvitationState::Revoked
    } else if expired {
        InvitationState::Expired
    } else {
        InvitationState::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adult_is_off_by_default_and_modes_default_to_approval() {
        for category in Category::ALL {
            let e = Entitlement::default_for(category);
            assert!(!e.enabled);
            assert_eq!(e.request_mode, RequestMode::Approval);
        }
        assert_eq!(RequestMode::default(), RequestMode::Approval);
    }

    #[test]
    fn complete_entitlements_fills_defaults_in_order() {
        let given = [Entitlement {
            category: Category::Music,
            enabled: true,
            request_mode: RequestMode::Auto,
        }];
        let all = complete_entitlements(&given);
        assert_eq!(
            all.iter().map(|e| e.category).collect::<Vec<_>>(),
            Category::ALL
        );
        assert_eq!(all[2], given[0]);
        assert_eq!(all[3], Entitlement::default_for(Category::Adult));
    }

    #[test]
    fn names_round_trip() {
        for c in Category::ALL {
            assert_eq!(Category::parse(c.as_str()), Some(c));
        }
        for s in [
            MemberStatus::Active,
            MemberStatus::Suspended,
            MemberStatus::Retired,
        ] {
            assert_eq!(MemberStatus::parse(s.as_str()), Some(s));
        }
        for m in [RequestMode::Auto, RequestMode::Approval] {
            assert_eq!(RequestMode::parse(m.as_str()), Some(m));
        }
        for p in [PasswordTokenPurpose::Set, PasswordTokenPurpose::Reset] {
            assert_eq!(PasswordTokenPurpose::parse(p.as_str()), Some(p));
        }
        assert_eq!(Category::parse("podcasts"), None);
    }

    #[test]
    fn emails_are_normalized_and_loosely_checked() {
        assert_eq!(parse_email(" Mia@Example.ORG ").unwrap(), "mia@example.org");
        for bad in [
            "",
            "mia",
            "@example.org",
            "mia@",
            "mia@example",
            "mia@.org",
            "mia@example.",
            "m ia@example.org",
            "a@b@example.org",
        ] {
            assert_eq!(parse_email(bad), Err(EmailError), "{bad}");
        }
        let long = format!("{}@example.org", "a".repeat(250));
        assert_eq!(parse_email(&long), Err(EmailError));
    }

    #[test]
    fn invitation_state_prefers_final_states() {
        use InvitationState::*;
        assert_eq!(invitation_state(false, false, false), Pending);
        assert_eq!(invitation_state(false, false, true), Expired);
        assert_eq!(invitation_state(false, true, true), Revoked);
        assert_eq!(invitation_state(true, false, true), Accepted);
    }
}
