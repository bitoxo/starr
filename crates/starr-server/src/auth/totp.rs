//! Second factor for admins (ADR 0005): TOTP per RFC 6238 (SHA-1, 6 digits,
//! 30-second steps, one step of clock skew either way, what every
//! authenticator app understands) plus one-time recovery codes.
//!
//! TOTP secrets are stored sealed by [`TotpSecretBox`]: `age` (via
//! `starr-secrets`, purpose `TotpSecrets`) to the web process's own key
//! (`<data_dir>/web.key`), separate from the worker's key for service
//! credentials (ADR 0003). The sealed plaintext names the admin's row id, so
//! a sealed secret copied to another row does not open.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use std::sync::Arc;

use secrecy::{ExposeSecret, SecretSlice, SecretString};
use sha2::{Digest, Sha256};
use starr_core::auth::{RECOVERY_CODE_ALPHABET, RECOVERY_CODE_CHARS, RECOVERY_CODE_COUNT};
use starr_secrets::{SealedSecret, TotpKey};
use totp_rs::{Builder, Totp};

/// Name shown in authenticator apps.
pub const ISSUER: &str = "Starr";
/// 160-bit secrets, as RFC 4226 recommends.
const SECRET_BYTES: usize = 20;
/// Separates the recipient from the age ciphertext in the stored blob.
const RECIPIENT_END: u8 = b'\n';

/// A raw TOTP secret. Never printed; zeroed on drop.
#[derive(Debug)]
pub struct TotpSecret(SecretSlice<u8>);

impl TotpSecret {
    /// A fresh random secret.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = vec![0u8; SECRET_BYTES];
        rand::fill(&mut bytes[..]);
        Self(SecretSlice::from(bytes))
    }

    fn totp(&self, account: &str) -> Option<Totp> {
        Builder::new()
            .with_secret(self.0.expose_secret())
            .with_account_name(account)
            .with_issuer(Some(ISSUER))
            .build()
            .ok()
    }

    /// The secret in base32, for typing it into an app by hand.
    #[must_use]
    pub fn base32(&self) -> SecretString {
        SecretString::from(totp_rs::Secret::from(self.0.expose_secret()).to_base32())
    }

    /// `otpauth://totp/...` URI for `account` (the username).
    #[must_use]
    pub fn otpauth_uri(&self, account: &str) -> Option<SecretString> {
        self.totp(account)?.to_url().ok().map(SecretString::from)
    }

    /// The time step a code is valid for at `now` (current step ± 1), or
    /// `None` if the code is wrong. Accepting the step only once is the
    /// caller's job (replay protection).
    #[must_use]
    pub fn matching_step(&self, code: &str, now: DateTime<Utc>) -> Option<i64> {
        let time = u64::try_from(now.timestamp()).ok()?;
        let step = self.totp("")?.check(code, time)?;
        i64::try_from(step).ok()
    }

    /// The code for `now` (tests and the recovery flow's examples).
    #[must_use]
    pub fn code_at(&self, now: DateTime<Utc>) -> Option<String> {
        let time = u64::try_from(now.timestamp()).ok()?;
        Some(self.totp("")?.generate(time).to_string())
    }
}

/// Seals and opens TOTP secrets with the web process's key.
///
/// Stored blob: the age recipient, a newline, the age ciphertext. The
/// plaintext is `<admin id>:<base32 secret>`.
#[derive(Clone)]
pub struct TotpSecretBox {
    key: Arc<TotpKey>,
}

impl std::fmt::Debug for TotpSecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TotpSecretBox")
            .field("recipient", &self.key.recipient())
            .finish_non_exhaustive()
    }
}

impl TotpSecretBox {
    #[must_use]
    pub fn new(key: TotpKey) -> Self {
        Self { key: Arc::new(key) }
    }

    /// Seal `secret` for the admin with row id `admin_id`.
    #[must_use]
    pub fn seal(&self, admin_id: i64, secret: &TotpSecret) -> Option<Vec<u8>> {
        let plain = SecretString::from(format!("{admin_id}:{}", secret.base32().expose_secret()));
        let (recipient, ciphertext) = self.key.seal(&plain).ok()?.into_parts();
        let mut stored = recipient.into_bytes();
        stored.push(RECIPIENT_END);
        stored.extend_from_slice(&ciphertext);
        Some(stored)
    }

    /// Open a sealed secret of admin `admin_id`. `None` if it was sealed for
    /// another admin, with another key, or tampered with.
    #[must_use]
    pub fn open(&self, admin_id: i64, stored: &[u8]) -> Option<TotpSecret> {
        let split = stored.iter().position(|b| *b == RECIPIENT_END)?;
        let recipient = std::str::from_utf8(&stored[..split]).ok()?;
        let sealed = SealedSecret::from_parts(recipient.to_owned(), stored[split + 1..].to_vec());
        let plain = self.key.open(&sealed).ok()?;
        let (owner, base32) = plain.expose_secret().split_once(':')?;
        if owner != admin_id.to_string() {
            return None;
        }
        let bytes = totp_rs::Secret::try_from_base32(base32).ok()?;
        Some(TotpSecret(SecretSlice::from(bytes.as_bytes().to_vec())))
    }
}

/// QR code of `uri` as an SVG image, base64 for a `data:` URL. Dark modules
/// on white with a quiet zone, as scanners expect.
#[must_use]
pub fn qr_svg_base64(uri: &SecretString) -> Option<String> {
    let code = qrcode::QrCode::new(uri.expose_secret().as_bytes()).ok()?;
    let svg = code
        .render::<qrcode::render::svg::Color<'_>>()
        .min_dimensions(240, 240)
        .quiet_zone(true)
        .build();
    Some(STANDARD.encode(svg))
}

/// Fresh recovery codes for display (`XXXX-XXXX-XXXX`) and their hashes
/// for storage.
#[must_use]
pub fn new_recovery_codes(admin_id: i64) -> (Vec<SecretString>, Vec<[u8; 32]>) {
    (0..RECOVERY_CODE_COUNT)
        .map(|_| {
            let mut bytes = [0u8; RECOVERY_CODE_CHARS];
            rand::fill(&mut bytes[..]);
            // 32 symbols: the low five bits pick one uniformly.
            let code: String = bytes
                .iter()
                .map(|b| char::from(RECOVERY_CODE_ALPHABET[usize::from(b & 31)]))
                .collect();
            let hash = hash_recovery_code(admin_id, &code);
            let display = format!("{}-{}-{}", &code[..4], &code[4..8], &code[8..]);
            (SecretString::from(display), hash)
        })
        .unzip()
}

/// Hash of a normalized recovery code (uppercase, no separators), salted
/// with the admin's id so leaked hashes cannot be attacked all at once.
/// Codes carry 60 random bits, so a fast hash suffices: they are useless
/// without the password, which is Argon2-hashed.
#[must_use]
pub fn hash_recovery_code(admin_id: i64, normalized: &str) -> [u8; 32] {
    Sha256::new()
        .chain_update(b"starr/recovery-code/v1:")
        .chain_update(admin_id.to_be_bytes())
        .chain_update(normalized.as_bytes())
        .finalize()
        .into()
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use starr_core::auth::{SecondFactor, parse_second_factor};

    use super::*;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn rfc_6238_sha1_test_vector() {
        // RFC 6238 Appendix B: secret "12345678901234567890", T = 59 s.
        let secret = TotpSecret(SecretSlice::from(b"12345678901234567890".to_vec()));
        let at = DateTime::from_timestamp(59, 0).unwrap();
        assert_eq!(secret.code_at(at).unwrap(), "287082");
        assert_eq!(secret.matching_step("287082", at), Some(1));
    }

    #[test]
    fn codes_match_one_step_of_skew_either_way() {
        let secret = TotpSecret::generate();
        let code = secret.code_at(t0()).unwrap();
        let step = t0().timestamp() / 30;
        assert_eq!(secret.matching_step(&code, t0()), Some(step));
        assert_eq!(
            secret.matching_step(&code, t0() + TimeDelta::seconds(30)),
            Some(step)
        );
        assert_eq!(
            secret.matching_step(&code, t0() - TimeDelta::seconds(30)),
            Some(step)
        );
        assert_eq!(
            secret.matching_step(&code, t0() + TimeDelta::seconds(90)),
            None
        );
        assert_eq!(secret.matching_step("not-a-code", t0()), None);
    }

    #[test]
    fn otpauth_uri_names_issuer_and_account() {
        let secret = TotpSecret::generate();
        let uri = secret.otpauth_uri("root").unwrap();
        let uri = uri.expose_secret();
        assert!(uri.starts_with("otpauth://totp/Starr:root?"), "{uri}");
        assert!(uri.contains(&format!("secret={}", secret.base32().expose_secret())));
        assert!(uri.contains("issuer=Starr"), "{uri}");
        let svg = STANDARD
            .decode(qr_svg_base64(&SecretString::from(uri.to_owned())).unwrap())
            .unwrap();
        assert!(String::from_utf8(svg).unwrap().contains("<svg"));
    }

    #[test]
    fn sealed_secrets_open_only_for_their_admin_and_key() {
        let boxed = TotpSecretBox::new(TotpKey::generate());
        let secret = TotpSecret::generate();
        let sealed = boxed.seal(1, &secret).unwrap();
        assert_ne!(boxed.seal(1, &secret).unwrap(), sealed, "fresh nonce");
        let opened = boxed.open(1, &sealed).unwrap();
        assert_eq!(opened.code_at(t0()), secret.code_at(t0()));
        assert!(boxed.open(2, &sealed).is_none(), "bound to the admin");
        assert!(
            TotpSecretBox::new(TotpKey::generate())
                .open(1, &sealed)
                .is_none()
        );
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(boxed.open(1, &tampered).is_none());
        assert!(boxed.open(1, &[]).is_none());
        assert!(boxed.open(1, &sealed[..10]).is_none());
        // Neither the raw nor the base32 secret appears in the sealed bytes.
        let raw = secret.0.expose_secret();
        assert!(!sealed.windows(raw.len()).any(|w| w == raw));
        let b32 = secret.base32();
        let b32 = b32.expose_secret().as_bytes();
        assert!(!sealed.windows(b32.len()).any(|w| w == b32));
    }

    #[test]
    fn recovery_codes_are_distinct_and_parse_back() {
        let (codes, hashes) = new_recovery_codes(1);
        assert_eq!(codes.len(), RECOVERY_CODE_COUNT);
        let mut unique = hashes.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), RECOVERY_CODE_COUNT);
        for (code, hash) in codes.iter().zip(&hashes) {
            let Some(SecondFactor::RecoveryCode(normalized)) =
                parse_second_factor(&code.expose_secret().to_lowercase())
            else {
                panic!("{} does not parse", code.expose_secret());
            };
            assert_eq!(&hash_recovery_code(1, &normalized), hash);
            assert_ne!(&hash_recovery_code(2, &normalized), hash);
        }
    }
}
