//! Password hashing (Argon2id) and opaque tokens (session, setup).
//!
//! Hashing and verifying are CPU-heavy on purpose: call them via
//! `tokio::task::spawn_blocking`.

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// OWASP Password Storage Cheat Sheet: Argon2id, m = 19 MiB, t = 2, p = 1.
const ARGON2_MEMORY_KIB: u32 = 19 * 1024;
const ARGON2_ITERATIONS: u32 = 2;
const ARGON2_PARALLELISM: u32 = 1;

/// Hash of a random, discarded password with the parameters above. Verifying
/// against it for unknown usernames makes their timing match real accounts.
pub(crate) const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$S/FQSeB9gA0fL8BggAtMog$qJCO3ijpLSbtMaecBINrLy4oklMo0v8tSEJpzgqtxKg";

/// Random bytes per token: 256 bits.
const TOKEN_BYTES: usize = 32;

/// SHA-256 of a token: what is stored and compared.
pub type TokenHash = [u8; 32];

#[derive(Debug, thiserror::Error)]
#[error("password hashing failed: {0}")]
pub struct HashError(String);

fn argon2() -> Result<Argon2<'static>, HashError> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_ITERATIONS,
        ARGON2_PARALLELISM,
        None,
    )
    .map_err(|e| HashError(e.to_string()))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// Hash a password into a PHC string with a fresh random salt.
pub fn hash_password(password: &SecretString) -> Result<String, HashError> {
    argon2()?
        .hash_password(password.expose_secret().as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|e| HashError(e.to_string()))
}

/// Verify a password against a PHC string. Malformed hashes never verify.
#[must_use]
pub fn verify_password(password: &SecretString, phc: &str) -> bool {
    let Ok(hash) = PasswordHash::new(phc) else {
        return false;
    };
    let Ok(argon2) = argon2() else {
        return false;
    };
    argon2
        .verify_password(password.expose_secret().as_bytes(), &hash)
        .is_ok()
}

/// A new random token, URL-safe base64 (43 characters).
#[must_use]
pub fn new_token() -> SecretString {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::fill(&mut bytes[..]);
    SecretString::from(URL_SAFE_NO_PAD.encode(bytes))
}

#[must_use]
pub fn hash_token(token: &str) -> TokenHash {
    Sha256::digest(token.as_bytes()).into()
}

/// Constant-time equality for token hashes.
#[must_use]
pub fn hashes_equal(a: &TokenHash, b: &TokenHash) -> bool {
    a.ct_eq(b).into()
}

type HmacSha256 = Hmac<Sha256>;

fn hmac(key: &[u8; 32]) -> HmacSha256 {
    // HMAC zero-pads keys to the block size (64 bytes for SHA-256), so this is
    // the same MAC as keying with the 32 bytes, without a fallible constructor.
    let mut block = [0u8; 64];
    block[..32].copy_from_slice(key);
    HmacSha256::new(&block.into())
}

/// HMAC-SHA256 of `message`.
#[must_use]
pub fn hmac_sha256(key: &[u8; 32], message: &[u8]) -> [u8; 32] {
    hmac(key)
        .chain_update(message)
        .finalize()
        .into_bytes()
        .into()
}

/// Whether `tag` is the HMAC-SHA256 of `message`, compared in constant time.
#[must_use]
pub fn verify_hmac_sha256(key: &[u8; 32], message: &[u8], tag: &[u8]) -> bool {
    hmac(key).chain_update(message).verify_slice(tag).is_ok()
}

/// Throttle key for a submitted username: a hash, so a password typed into
/// the username field is never stored in clear.
#[must_use]
pub fn username_key(normalized: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(normalized.as_bytes()))
}

/// 32 random bytes, URL-safe base64.
#[must_use]
pub fn random_key_b64() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes[..]);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decode a key produced by [`random_key_b64`].
#[must_use]
pub fn decode_key(b64: &str) -> Option<[u8; 32]> {
    URL_SAFE_NO_PAD.decode(b64).ok()?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify_round_trip() {
        let password = SecretString::from("correct horse battery staple");
        let hash = hash_password(&password).unwrap();
        assert!(
            hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "{hash}"
        );
        assert!(verify_password(&password, &hash));
        assert!(!verify_password(&SecretString::from("wrong"), &hash));
        assert!(!verify_password(&password, "not a hash"));
    }

    #[test]
    fn dummy_hash_uses_current_parameters() {
        let current = hash_password(&SecretString::from("x")).unwrap();
        let params = |h: &str| h.rsplitn(3, '$').nth(2).unwrap().to_owned();
        assert_eq!(params(DUMMY_HASH), params(&current));
        assert!(PasswordHash::new(DUMMY_HASH).is_ok());
    }

    #[test]
    fn hmac_matches_rfc_4231_style_vector() {
        // Key of 32 bytes 0x0b, message "Hi There"; reference from Python's hmac module.
        let mac = hmac_sha256(&[0x0b; 32], b"Hi There");
        assert_eq!(
            URL_SAFE_NO_PAD.encode(mac),
            "GYpgfrRL-8aZA6Dxzyu9xboKo_PZrjwcejsWlqC2jPc"
        );
        assert!(verify_hmac_sha256(&[0x0b; 32], b"Hi There", &mac));
        assert!(!verify_hmac_sha256(&[0x0b; 32], b"Hi there", &mac));
        assert!(!verify_hmac_sha256(&[0x0b; 32], b"Hi There", &mac[..31]));
    }

    #[test]
    fn keys_round_trip() {
        let key = random_key_b64();
        assert!(decode_key(&key).is_some());
        assert!(decode_key("short").is_none());
    }

    #[test]
    fn tokens_are_random_and_hash_stably() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.expose_secret().len(), 43);
        assert_ne!(a.expose_secret(), b.expose_secret());
        let ha = hash_token(a.expose_secret());
        assert!(hashes_equal(&ha, &hash_token(a.expose_secret())));
        assert!(!hashes_equal(&ha, &hash_token(b.expose_secret())));
    }
}
