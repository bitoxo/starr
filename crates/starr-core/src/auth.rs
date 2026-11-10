//! Authentication policy (ADR 0005): identities, usernames, session lifetimes
//! and login throttling limits. Pure rules; storage and crypto live elsewhere.

use chrono::{DateTime, TimeDelta, Utc};
use serde::Serialize;

/// Absolute session lifetime, counted from login.
pub const SESSION_ABSOLUTE_LIFETIME: TimeDelta = TimeDelta::days(30);
/// A session unused for this long is dead.
pub const SESSION_IDLE_TIMEOUT: TimeDelta = TimeDelta::days(14);
/// `last_seen_at` is written at most this often per session.
pub const SESSION_TOUCH_INTERVAL: TimeDelta = TimeDelta::minutes(5);

/// Window in which failed logins are counted.
pub const THROTTLE_WINDOW: TimeDelta = TimeDelta::minutes(15);
/// Failed logins per username within [`THROTTLE_WINDOW`] before throttling.
/// Counts only attempts without a valid device cookie for that username, so
/// an attacker cannot lock out a user on a known device (OWASP device cookies).
pub const MAX_FAILURES_PER_USERNAME: i64 = 5;
/// Failed logins per device cookie within [`THROTTLE_WINDOW`].
pub const MAX_FAILURES_PER_DEVICE: i64 = 5;
/// Lifetime of a device cookie, renewed on every successful login.
pub const DEVICE_COOKIE_LIFETIME: TimeDelta = TimeDelta::days(180);
/// Failed logins per client IP within [`THROTTLE_WINDOW`] before throttling.
pub const MAX_FAILURES_PER_IP: i64 = 20;

/// Failed uses of public token endpoints (invitation, password link) per
/// client IP within [`THROTTLE_WINDOW`]. Counted apart from logins.
pub const MAX_TOKEN_FAILURES_PER_IP: i64 = 20;
/// Failed uses of one token within [`THROTTLE_WINDOW`].
pub const MAX_FAILURES_PER_TOKEN: i64 = 5;

/// Minimum password length in characters (maintainer decision 2026-11-01:
/// length only, no composition rules).
pub const PASSWORD_MIN_CHARS: usize = 8;
/// Maximum password length in characters.
pub const PASSWORD_MAX_CHARS: usize = 256;
/// Maximum display name length in characters.
pub const DISPLAY_NAME_MAX_CHARS: usize = 64;

/// Time between the password step and the TOTP step of a login.
pub const PENDING_LOGIN_LIFETIME: TimeDelta = TimeDelta::minutes(5);
/// After re-authenticating (`/auth/elevate`), an admin in member view may
/// switch back to the admin account for this long.
pub const ELEVATION_WINDOW: TimeDelta = TimeDelta::minutes(5);
/// The private category (adult) closes by itself after this long without
/// adult activity in the session (spec 0007 Q7 B); logout closes it too.
pub const PRIVATE_UNLOCK_IDLE: TimeDelta = TimeDelta::minutes(30);

/// Whether a session whose unlock runs `until` is unlocked at `now`.
#[must_use]
pub fn private_unlocked(until: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    until.is_some_and(|until| now < until)
}

/// Recovery codes issued when TOTP is enabled.
pub const RECOVERY_CODE_COUNT: usize = 10;
/// Characters of a recovery code (without separators).
pub const RECOVERY_CODE_CHARS: usize = 12;
/// Alphabet of recovery codes: 32 symbols (5 bits each), without the easily
/// confused `0`, `1`, `I` and `O`.
pub const RECOVERY_CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// A submitted second factor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecondFactor {
    /// Six-digit TOTP code.
    Totp(String),
    /// Recovery code, normalized (uppercase, no separators).
    RecoveryCode(String),
}

/// Read what someone typed into the code field: six digits are a TOTP code,
/// otherwise a recovery code (case and separators ignored). `None` if it is
/// neither.
#[must_use]
pub fn parse_second_factor(input: &str) -> Option<SecondFactor> {
    let compact: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    if compact.len() == 6 && compact.chars().all(|c| c.is_ascii_digit()) {
        return Some(SecondFactor::Totp(compact));
    }
    let upper = compact.to_ascii_uppercase();
    (upper.len() == RECOVERY_CODE_CHARS
        && upper.bytes().all(|b| RECOVERY_CODE_ALPHABET.contains(&b)))
    .then_some(SecondFactor::RecoveryCode(upper))
}

/// Why an admin account change was refused (domain model §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdminChangeError {
    #[error("admins cannot suspend or remove themselves")]
    SelfTarget,
    #[error("the acting admin is not active")]
    ActorInactive,
    #[error("at least one active admin must remain")]
    LastActiveAdmin,
}

/// Admins and members are separate identities; a session belongs to one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SubjectKind {
    Admin,
    Member,
}

impl SubjectKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "admin" => Some(Self::Admin),
            "member" => Some(Self::Member),
            _ => None,
        }
    }
}

/// An authenticated identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub kind: SubjectKind,
    pub id: i64,
    pub username: String,
    pub display_name: String,
}

/// Why a username was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UsernameError {
    #[error(
        "username must be 3 to 32 characters: a-z, 0-9, '.', '_' or '-', starting with a letter or digit"
    )]
    Invalid,
}

/// Why a new password was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PasswordError {
    #[error("password must have at least {PASSWORD_MIN_CHARS} characters")]
    TooShort,
    #[error("password must have at most {PASSWORD_MAX_CHARS} characters")]
    TooLong,
}

/// The password policy, applied wherever a password is set (admins and
/// members alike). Length only, counted in characters.
pub fn validate_password(password: &str) -> Result<(), PasswordError> {
    let chars = password.chars().count();
    if chars < PASSWORD_MIN_CHARS {
        Err(PasswordError::TooShort)
    } else if chars > PASSWORD_MAX_CHARS {
        Err(PasswordError::TooLong)
    } else {
        Ok(())
    }
}

/// Normalize a username for storage and lookup: trimmed and lowercase.
#[must_use]
pub fn normalize_username(input: &str) -> String {
    input.trim().to_lowercase()
}

/// Normalize and validate a username chosen for a new account.
pub fn parse_username(input: &str) -> Result<String, UsernameError> {
    let name = normalize_username(input);
    let len = name.chars().count();
    let mut chars = name.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
    let rest_ok = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c));
    if (3..=32).contains(&len) && first_ok && rest_ok {
        Ok(name)
    } else {
        Err(UsernameError::Invalid)
    }
}

/// Whether a stored session may still be used at `now`, and whether its
/// `last_seen_at` should be refreshed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionValidity {
    Valid { touch: bool },
    Expired,
}

/// Apply absolute and idle lifetime to a session.
#[must_use]
pub fn session_validity(
    expires_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> SessionValidity {
    if now >= expires_at || now >= last_seen_at + SESSION_IDLE_TIMEOUT {
        SessionValidity::Expired
    } else {
        SessionValidity::Valid {
            touch: now - last_seen_at >= SESSION_TOUCH_INTERVAL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_unlock_holds_until_its_end() {
        let until = t(30);
        assert!(!private_unlocked(None, t(0)));
        assert!(private_unlocked(Some(until), t(29)));
        assert!(!private_unlocked(Some(until), t(30)));
        assert!(!private_unlocked(Some(until), t(31)));
        assert_eq!(PRIVATE_UNLOCK_IDLE, TimeDelta::minutes(30));
    }

    fn t(minutes: i64) -> DateTime<Utc> {
        DateTime::UNIX_EPOCH + TimeDelta::minutes(minutes)
    }

    #[test]
    fn usernames_are_normalized_and_validated() {
        assert_eq!(parse_username("  Alice.B ").unwrap(), "alice.b");
        assert_eq!(parse_username("a_1-b").unwrap(), "a_1-b");
        for bad in [
            "ab",
            "_alice",
            "al ice",
            "älice",
            "a@b",
            &"a".repeat(33),
            "",
        ] {
            assert_eq!(parse_username(bad), Err(UsernameError::Invalid), "{bad}");
        }
    }

    #[test]
    fn second_factor_input_is_a_totp_code_or_a_recovery_code() {
        assert_eq!(
            parse_second_factor(" 123 456 "),
            Some(SecondFactor::Totp("123456".into()))
        );
        assert_eq!(
            parse_second_factor("abcd-efgh-jk23"),
            Some(SecondFactor::RecoveryCode("ABCDEFGHJK23".into()))
        );
        for bad in [
            "",
            "12345",
            "1234567",
            "ABCD-EFGH-JK2",
            "ABCD-EFGH-JK20",
            "abcdefghjk2!",
        ] {
            assert_eq!(parse_second_factor(bad), None, "{bad}");
        }
    }

    #[test]
    fn password_policy_is_length_only() {
        assert_eq!(validate_password("1234567"), Err(PasswordError::TooShort));
        assert_eq!(validate_password("12345678"), Ok(()));
        assert_eq!(validate_password("ääääääää"), Ok(()));
        assert_eq!(validate_password(&"x".repeat(256)), Ok(()));
        assert_eq!(
            validate_password(&"x".repeat(257)),
            Err(PasswordError::TooLong)
        );
    }

    #[test]
    fn fresh_session_is_valid_without_touch() {
        let start = t(0);
        let expires = start + SESSION_ABSOLUTE_LIFETIME;
        assert_eq!(
            session_validity(expires, start, t(1)),
            SessionValidity::Valid { touch: false }
        );
        assert_eq!(
            session_validity(expires, start, t(5)),
            SessionValidity::Valid { touch: true }
        );
    }

    #[test]
    fn idle_timeout_expires_session() {
        let start = t(0);
        let expires = start + SESSION_ABSOLUTE_LIFETIME;
        let idle_edge = start + SESSION_IDLE_TIMEOUT;
        assert!(matches!(
            session_validity(expires, start, idle_edge - TimeDelta::seconds(1)),
            SessionValidity::Valid { .. }
        ));
        assert_eq!(
            session_validity(expires, start, idle_edge),
            SessionValidity::Expired
        );
    }

    #[test]
    fn absolute_lifetime_wins_over_activity() {
        let start = t(0);
        let expires = start + SESSION_ABSOLUTE_LIFETIME;
        let now = expires;
        assert_eq!(
            session_validity(expires, now - TimeDelta::minutes(1), now),
            SessionValidity::Expired
        );
    }
}
